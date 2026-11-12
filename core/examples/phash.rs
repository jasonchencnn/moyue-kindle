fn main() {
    let p = std::env::args().nth(1).expect("path");
    let mut f = std::fs::File::open(p).unwrap();
    println!("{}", inkread_core::hash::partial_md5(&mut f).unwrap());
}
