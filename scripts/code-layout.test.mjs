import assert from 'node:assert/strict';
import {spawnSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';
import test from 'node:test';

const directory=fileURLToPath(new URL('./code-layout/',import.meta.url));

test('Linux layout relinks the same native inputs and preserves executable behavior', {
  skip:process.platform!=='linux'||process.arch!=='x64',
  timeout:180_000,
},()=>{
  const result=spawnSync('python3',['-',directory],{encoding:'utf8',timeout:170_000,input:String.raw`
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
sys.path.insert(0, sys.argv[1])
import build
from train import begin, finish, terminate

with tempfile.TemporaryDirectory(prefix='dim-layout-test-') as temporary:
    root = Path(temporary)
    source = root / 'main.c'
    source.write_text('''#include <stdio.h>
__attribute__((noinline)) int first(void) { return 17; }
int first_alias(void) __attribute__((alias("first")));
__attribute__((noinline)) int second(void) { return 25; }
__attribute__((noinline)) static int shared_name(void) { return 0; }
__attribute__((noinline)) static int discarded_name(void) { return 0; }
int main(void) { printf("%d\\n", first() + second() + shared_name() + discarded_name()); return 0; }
''')
    library_source = root / 'shared.c'
    library_source.write_text('int shared_name(void) { return 7; }\n')
    library = root / 'shared.so'
    subprocess.run(['cc','-shared','-fPIC',str(library_source),'-o',str(library)],check=True)
    obj = root / 'main.o'
    spare_source = root / 'spare.c'
    spare_source.write_text('__attribute__((used,noinline)) static int discarded_name(void) { return 7; }\n')
    spare = root / 'spare.o'
    subprocess.run(['cc','-O0','-ffunction-sections','-c',str(spare_source),'-o',str(spare)],check=True)
    base = root / 'base'
    subprocess.run(['cc', '-O0', '-ffunction-sections', '-c', str(source), '-o', str(obj)], check=True)
    sysroot = Path(subprocess.check_output(['rustc','--print','sysroot'],text=True).strip())
    linker_dir = sysroot / 'lib/rustlib/x86_64-unknown-linux-gnu/bin/gcc-ld'
    record = {'compiler':'cc', 'arguments':['-B'+str(linker_dir), '-fuse-ld=lld', str(obj),
              str(spare), '-Wl,--gc-sections', '-Wl,--no-as-needed', str(library), '-Wl,--as-needed', '-o', str(base)],
              'cwd':str(root), 'path':os.environ['PATH']}
    build.relink(record, base)
    original = build.symbols(base)
    order = root / 'order'
    order.write_text('second\nfirst_alias\nfirst\nmain\n')
    changed = root / 'changed'
    build.relink(record, changed, order)
    reordered = build.symbols(changed)
    assert sorted((n,e-s) for s,e,n in original) == sorted((n,e-s) for s,e,n in reordered)
    addresses = {n:s for s,e,n in reordered}
    assert addresses['second'] < addresses['first'] < addresses['main']
    assert addresses['first_alias'] == addresses['first']
    assert build.validate_layout(base, changed, order)['movedFunctionSymbols'] > 0
    try:
        build.validate_layout(base, base, order)
    except RuntimeError as error:
        assert 'do not follow' in str(error)
    else:
        raise AssertionError('an unordered executable must not pass layout acceptance')
    unchanged_order = root / 'unchanged.order'
    unchanged_order.write_text('\n'.join(n for _,_,n in original if n in ('first','second','main'))+'\n')
    try:
        build.validate_layout(base, base, unchanged_order)
    except RuntimeError as error:
        assert 'moved no selected' in str(error)
    else:
        raise AssertionError('an unchanged layout must not be published as an optimized artifact')
    assert subprocess.check_output([str(base)]) == subprocess.check_output([str(changed)]) == b'42\n'
    probe = root / 'probe.so'
    subprocess.run(['cc','-shared','-fPIC','-O2','-Wall','-Wextra','-Werror',
                    str(build.HERE/'probe.c'),'-ldl','-o',str(probe)],check=True)
    process = subprocess.Popen([str(base)], env=dict(os.environ,LD_PRELOAD=str(probe)),
                               stdout=subprocess.PIPE, start_new_session=True)
    try:
        begin(process)
        profile = root / 'lifecycle.smaps'
        finish(process, profile)
        assert process.stdout.read() == b'42\n'
        measurement = build.order(base, [profile,profile,profile], root/'trained.order')
        assert measurement['orderedFunctions'] > 0
        trained = root / 'trained'
        build.ordered_link(record, trained, root/'trained.order')
        assert subprocess.check_output([str(trained)]) == b'42\n'
    finally:
        terminate(process)
        process.stdout.close()

    project = root / 'rust'
    project.mkdir()
    (project / 'src').mkdir()
    (project / 'Cargo.toml').write_text('''[package]
name = "dimcode"
version = "0.0.0"
edition = "2024"
[profile.release]
lto = "fat"
codegen-units = 1
strip = "debuginfo"
''')
    (project / 'src/main.rs').write_text('''#[unsafe(no_mangle)]
#[inline(never)]
pub extern "C" fn layout_first(x: u32) -> u32 { x.wrapping_add(17) }
#[unsafe(no_mangle)]
#[inline(never)]
pub extern "C" fn layout_second(x: u32) -> u32 { x ^ 25 }
fn main() { let x = std::hint::black_box(0); println!("{}", layout_first(x) + layout_second(x)); }
''')
    shutil.copyfile(build.REPO / 'rust-toolchain.toml', project / 'rust-toolchain.toml')
    for name in list(os.environ):
        if name.startswith('CARGO_') or name in ('RUSTFLAGS', 'RUSTC', 'RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER'):
            del os.environ[name]
    os.environ['CARGO_HOME'] = str(root / 'cargo-home')
    subprocess.run(['cargo','generate-lockfile','--offline'],cwd=project,check=True)
    capture = root / 'capture'
    capture.mkdir()
    build.REPO = project
    recorded = build.cargo_link(capture, shutil.which('cc'), 2)
    rust_base = root / 'rust-base'
    build.relink(recorded, rust_base)
    assert build.digest(rust_base) == build.digest(Path(recorded['output']))
    rust_names = [name for _,_,name in build.symbols(rust_base) if name in ('layout_first','layout_second')]
    assert len(rust_names) == 2
    rust_order = root / 'rust.order'
    rust_order.write_text('\n'.join(reversed(rust_names))+'\n')
    rust_changed = root / 'rust-changed'
    build.ordered_link(recorded, rust_changed, rust_order)
    assert build.validate_layout(rust_base, rust_changed, rust_order)['movedFunctionSymbols'] > 0
    assert subprocess.check_output([str(rust_base)]) == subprocess.check_output([str(rust_changed)]) == b'42\n'

    selected_sysroot = root / 'selected-toolchain'
    selected_sysroot.symlink_to(sysroot, target_is_directory=True)
    selected_rustc = root / 'selected rustc'
    compilation_marker = root / 'compiler-ran'
    selected_rustc.write_text(f'''#!{sys.executable}
import os, sys
from pathlib import Path
if sys.argv[1:] == ['--print', 'sysroot']:
    print({str(selected_sysroot)!r})
else:
    if '--crate-name' in sys.argv and sys.argv[sys.argv.index('--crate-name') + 1] == 'dimcode':
        Path({str(compilation_marker)!r}).write_text('compiled dimcode')
    os.execv({str(sysroot / 'bin/rustc')!r}, ['rustc', *sys.argv[1:]])
''')
    selected_rustc.chmod(0o700)
    for variable in ('RUSTC', 'CARGO_BUILD_RUSTC'):
        os.environ.pop('RUSTC', None)
        os.environ['CARGO_BUILD_RUSTC'] = str(root / 'unused-compiler')
        os.environ[variable] = str(selected_rustc)
        overridden = root / variable
        overridden.mkdir()
        compilation_marker.unlink(missing_ok=True)
        recorded = build.cargo_link(overridden, shutil.which('cc'), 2)
        assert compilation_marker.read_text() == 'compiled dimcode'
        assert '-B'+str(selected_sysroot / 'lib/rustlib/x86_64-unknown-linux-gnu/bin/gcc-ld') in recorded['arguments']
        assert recorded['rustc'] == subprocess.check_output([str(selected_rustc), '-vV'], text=True)
        replay = overridden / 'replay'
        build.relink(recorded, replay)
        assert build.digest(replay) == build.digest(Path(recorded['output']))
        assert subprocess.check_output([str(replay)]) == b'42\n'
`});
  assert.equal(result.error,undefined);
  assert.equal(result.status,0,result.stdout+result.stderr);
});
