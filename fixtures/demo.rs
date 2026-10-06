use std::time::Duration;

fn main() {
    let message = "Mellow ✦ terminal editing should feel modern";
    println!("{message}");
    std::thread::sleep(Duration::from_millis(10));
}
