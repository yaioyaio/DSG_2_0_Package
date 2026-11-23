use dsg_core::check_command;
fn main() {
    let cmd=std::env::args().skip(1).collect::<Vec<_>>().join(" ");
    println!("DSG Rust scaffold: {:?}",check_command(&cmd));
}
