use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = xtask_check::root();
    match std::env::args().nth(1).as_deref() {
        None | Some("check") => xtask_check::check_all(&root)?,
        Some("module-graph") => {
            let mut args = std::env::args().skip(2);
            let Some(out) = args.next() else {
                return Err("usage: cargo run -p xtask-check -- module-graph DIR".into());
            };
            if args.next().is_some() {
                return Err("module-graph takes one output directory".into());
            }
            let out = PathBuf::from(out);
            std::fs::create_dir_all(&out)?;
            for (name, text) in xtask_check::modules::generate(&root)? {
                std::fs::write(out.join(name), text)?;
            }
            println!("generated {}", out.display());
        }
        Some("types") => xtask_check::types::write(&root)?,
        _ => {
            return Err("usage: cargo run -p xtask-check -- [check|module-graph DIR|types]".into());
        }
    }
    Ok(())
}
