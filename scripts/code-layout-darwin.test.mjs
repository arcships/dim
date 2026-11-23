import assert from 'node:assert/strict';
import {spawnSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';
import test from 'node:test';

const directory=fileURLToPath(new URL('./code-layout/',import.meta.url));
const python=process.platform==='win32'?'python':'python3';

test('Mach-O artifact parsing preserves fixed names, raw map bytes and function counts',()=>{
  const result=spawnSync(python,['-',directory],{encoding:'utf8',input:String.raw`
from pathlib import Path
import struct
import sys
import tempfile
from unittest.mock import patch
sys.path.insert(0, sys.argv[1])
import build_darwin
import macho
import samples

with tempfile.TemporaryDirectory(prefix='dim-layout-starts-') as temporary:
    binary = Path(temporary)/'fixture'
    header = struct.pack('<IiiIIIII', macho.MH_MAGIC_64, 0x1000007, 3, 2, 1, 16, 0, 0)
    for payload, expected in ((bytes.fromhex('01 01 00 00 00 00 00 00'), 2),
                              (bytes.fromhex('81 01 01 00 00 00 00 00'), 2),
                              (bytes.fromhex('81 81 01 81 01 00 00 00'), 2),
                              (bytes(8), 0)):
        binary.write_bytes(header + struct.pack('<IIII', macho.LC_FUNCTION_STARTS, 16, 48, len(payload)) + payload)
        assert macho.function_start_count(binary) == expected, payload.hex()

    segment_size = 72 + 2*80
    header = struct.pack('<IiiIIIII', macho.MH_MAGIC_64, 0x1000007, 3, 2, 1, segment_size, 0, 0)
    segment = struct.pack('<II16sQQQQiiII', macho.LC_SEGMENT_64, segment_size, b'__TEXT',
                          4096, 4096, 0, 4096, 5, 5, 2, 0)
    def section(name, address, size, offset):
        return struct.pack('<16s16sQQIIIIIIII', name, b'__TEXT', address, size, offset, 4, 0, 0, 0, 0, 0, 0)
    binary.write_bytes(header + segment + section(b'__gcc_except_tab', 4160, 16, 576)
                       + section(b'__text', 4096, 64, 512))
    assert macho.text_range(binary) == (4096, 64, 512)
    map_path = Path(temporary)/'fixture.map'
    map_path.write_bytes(b'# Symbols:\n0x1000\t0x10\t[ 1] _entry\n'
                         b'0x1010\t0x10\t[ 1] _raw_\xc0\n'
                         b'0x2000\t0x10\t[ 1] literal string: \xc0\n')
    sample = Path(temporary)/'fixture.pages'
    order = Path(temporary)/'fixture.order'
    samples.write(sample, 4096, [0])
    with patch.object(macho, '_page_size', return_value=4096):
        build_darwin.order(binary, map_path, [sample], order)
    assert order.read_bytes() == b'_entry\n_raw_\xc0\n'
`});
  assert.equal(result.error,undefined);
  assert.equal(result.status,0,result.stdout+result.stderr);
});

test('macOS layout orders Mach-O functions and samples __text residency', {
  skip:process.platform!=='darwin',
  timeout:120_000,
},()=>{
  const result=spawnSync(python,['-',directory],{encoding:'utf8',timeout:115_000,input:String.raw`
import json
import os
from pathlib import Path
import select
import shutil
import signal
import subprocess
import sys
import tempfile
import time
sys.path.insert(0, sys.argv[1])
import build_darwin
import macho
import samples

artifacts = os.environ.get('DIM_LAYOUT_ARTIFACTS')
class Keep(tempfile.TemporaryDirectory):
    def cleanup(self):
        if artifacts:
            target = Path(artifacts)
            target.mkdir(parents=True, exist_ok=True)
            for pattern in ('*.map', '*.order', 'replay/*', 'capture/link.json'):
                for path in Path(self.name).glob(pattern):
                    shutil.copy2(path, target / path.name)
        super().cleanup()

with Keep(prefix='dim-layout-mac-') as temporary:
    root = Path(temporary)
    (root / 'fixture.c').write_text('''__attribute__((noinline)) int first(void) { return 17; }
__attribute__((noinline)) int second(void) { return 25; }
int main(void) { return first() + second(); }
''')
    obj = root / 'fixture.o'
    subprocess.run(['cc','-O0','-c',str(root/'fixture.c'),'-o',str(obj)],check=True)
    record = {'compiler':'cc','arguments':['-o',str(root/'base'),str(obj)],
              'cwd':str(root),'path':os.environ['PATH'], 'strip':'none', 'objcopy':None, 'linkEnv':{},
              'inputs':[], 'output':str(root/'base')}
    base = root/'base'; base_map = root/'base.map'
    build_darwin.relink(record, base, map_path=base_map)
    assert subprocess.run([str(base)]).returncode == 42
    entries = macho.map_symbols(base_map, *macho.text_range(base)[:2])
    assert {name for _,_,name in entries} >= {'_first','_second','_main'}
    assert macho.function_start_count(base) == len(entries), entries
    order = root/'order'
    order.write_text('_second\n_first\n_main\n')
    candidate = root/'candidate'; candidate_map = root/'candidate.map'
    assert build_darwin.ordered_link(record, candidate, order, candidate_map) == []
    assert subprocess.run([str(candidate)]).returncode == 42
    measurement = build_darwin.validate(base, candidate, base_map, candidate_map, order)
    assert measurement['movedFunctionSymbols'] > 0
    missing = root/'missing.order'
    missing.write_text('_absent\n_second\n_first\n_main\n')
    assert build_darwin.ordered_link(record, root/'missing', missing, root/'missing.map') == ['_absent']
    assert '_absent' not in missing.read_text()

    project = root/'rust-project'; (project/'src').mkdir(parents=True)
    (project/'Cargo.toml').write_text('''[package]
name = "dimcode"
version = "0.0.0"
edition = "2021"
[dependencies]
layout_dependency = { path = "../dependency" }
[profile.release]
lto = "fat"
codegen-units = 1
debug = 1
strip = "debuginfo"
''')
    (project/'src/main.rs').write_text('''#[inline(never)] fn layout_first(x: u32) -> u32 { std::hint::black_box(x).wrapping_add(3) }
#[inline(never)] fn layout_second(x: u32) -> u32 { std::hint::black_box(x).wrapping_mul(5) }
fn main() { assert_eq!(layout_first(layout_second(layout_dependency::value())), 13); println!("layout-rust"); }
''')
    sysroot = subprocess.check_output([build_darwin.rustc_command(), '--print', 'sysroot'], text=True).strip()
    dependency = root/'dependency'; (dependency/'src').mkdir(parents=True)
    (dependency/'Cargo.toml').write_text('[package]\nname = "layout_dependency"\nversion = "0.0.0"\nedition = "2021"\n')
    (dependency/'src/lib.rs').write_text('pub fn value() -> u32 { 2 }\n')
    (project/'rust-toolchain.toml').write_text('[toolchain]\npath = '+json.dumps(sysroot)+'\n')
    (dependency/'rust-toolchain.toml').write_text('[toolchain]\npath = '+json.dumps(str(root/'unselected-toolchain'))+'\n')
    os.environ.pop('RUSTUP_TOOLCHAIN', None)
    os.environ['RUSTC'] = shutil.which(build_darwin.rustc_command())
    subprocess.run([str(Path(sysroot)/'bin/cargo'),'generate-lockfile','--offline'], cwd=project, check=True)
    build_darwin.REPO = project
    capture = root/'capture'; capture.mkdir()
    recorded = build_darwin.cargo_link(capture, shutil.which('cc'), 1)
    assert recorded['strip'] == 'debuginfo'
    cargo = Path(recorded['output'])
    original = cargo.read_bytes()
    replay = root/'replay'; replay.mkdir()
    replayed, replay_map = build_darwin.replay_baseline(recorded, capture, replay)
    assert cargo.read_bytes() == original, 'replay must restore the original Cargo executable'
    assert replayed.read_bytes() == original, 'replay must include Rust post-link stripping'
    assert subprocess.check_output([str(replayed)], text=True).strip() == 'layout-rust'
    rust_symbols = macho.map_symbols(replay_map, *macho.text_range(replayed)[:2])
    selected = [name for _, _, name in rust_symbols if 'layout_first' in name or 'layout_second' in name]
    assert len(selected) == 2, selected
    rust_order = root/'rust.order'; rust_order.write_text('\n'.join(reversed(selected))+'\n')
    rust_candidate = root/'rust-candidate'; rust_map = root/'rust.map'
    assert build_darwin.ordered_link(recorded, rust_candidate, rust_order, rust_map) == []
    assert build_darwin.validate(replayed, rust_candidate, replay_map, rust_map, rust_order)['movedFunctionSymbols'] > 0
    assert subprocess.check_output([str(rust_candidate)], text=True).strip() == 'layout-rust'

    page_size = os.sysconf('SC_PAGESIZE')
    page_sample = root/'sample.pages'
    samples.write(page_sample, page_size, [0])
    training = build_darwin.order(base, base_map, [page_sample], root/'trained.order')
    assert training['orderedFunctions'] > 0
    assert '_main' in (root/'trained.order').read_text()

    probe = root/'probe.dylib'
    subprocess.run(['cc','-dynamiclib','-fPIC','-O2','-Wall','-Wextra','-Werror',
                    str(build_darwin.HERE/'probe_darwin.c'),'-o',str(probe)],check=True)
    exit_sample = root/'exit.pages'
    environment = dict(os.environ, DYLD_INSERT_LIBRARIES=str(probe),
                       DIM_LAYOUT_EXIT_SAMPLE=str(exit_sample))
    assert subprocess.run([str(base)], env=environment).returncode == 42
    observed, pages = samples.read(exit_sample)
    assert observed == page_size
    assert pages

    signal_sample = root/'signal.pages'
    sleeper = root/'sleeper'
    (root / 'sleeper.c').write_text('''#include <unistd.h>
int main(void) {
    if (write(STDOUT_FILENO, "R", 1) != 1)
        return 1;
    pause();
    return 0;
}
''')
    subprocess.run(['cc','-O0','-o',str(sleeper),str(root/'sleeper.c')],check=True)
    sleeper_error = root/'sleeper.stderr'
    with sleeper_error.open('wb') as stderr, subprocess.Popen(
        [str(sleeper)], env=dict(os.environ, DYLD_INSERT_LIBRARIES=str(probe),
                                DIM_LAYOUT_SAMPLE=str(signal_sample)),
        stdout=subprocess.PIPE, stderr=stderr,
    ) as process:
        try:
            readable, _, _ = select.select([process.stdout], [], [], 20)
            assert readable and os.read(process.stdout.fileno(), 1) == b'R', (
                'sleeper did not reach main', process.poll(), sleeper_error.read_text())
            process.send_signal(signal.SIGUSR1)
            deadline = time.monotonic() + 20
            while time.monotonic() < deadline:
                if signal_sample.exists() and signal_sample.stat().st_size:
                    break
                time.sleep(.02)
            assert signal_sample.exists() and signal_sample.stat().st_size, (
                process.poll(), sleeper_error.read_text())
        finally:
            process.kill()
`});
  assert.equal(result.error,undefined);
  assert.equal(result.status,0,result.stdout+result.stderr);
});
