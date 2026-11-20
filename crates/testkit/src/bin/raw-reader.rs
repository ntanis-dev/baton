//! The `raw-reader` program: reads the console in raw mode and logs what it gets
//! (`baton_testkit::raw_reader`).

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(baton_testkit::raw_reader::run(&argv));
}
