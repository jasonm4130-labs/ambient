use ort::session::Session;

fn dump(path: &str) -> ort::Result<()> {
    let s = Session::builder()?.commit_from_file(path)?;
    println!("\n=== {} ===", path.rsplit('/').next().unwrap());
    let m = s.metadata()?;
    for k in m.custom_keys()? {
        if let Some(v) = m.custom(&k) {
            let v = if v.len() > 120 {
                format!("{}…", &v[..120])
            } else {
                v
            };
            println!("  meta {k} = {v}");
        }
    }
    for i in s.inputs() {
        println!("  in  {:<20} {:?}", i.name(), i.dtype());
    }
    for o in s.outputs() {
        println!("  out {:<20} {:?}", o.name(), o.dtype());
    }
    Ok(())
}

fn main() -> ort::Result<()> {
    for p in std::env::args().skip(1) {
        dump(&p)?;
    }
    Ok(())
}
