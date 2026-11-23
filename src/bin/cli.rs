//! `bigtext-cli`: BigText's tools for scripts, without a window (filter, levels, count, split,
//! stats, merge, convert). A console program, so scripts get its output and exit code on
//! every platform; `bigtext --filter …` runs the same commands.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = if args.is_empty() {
        // Called wrongly: the usage goes where errors go, so a script's output stays clean.
        eprint!("{}", bigtext::headless::USAGE);
        2
    } else if matches!(args[0].as_str(), "-h" | "--help") {
        print!("{}", bigtext::headless::USAGE);
        0
    } else {
        bigtext::headless::run(
            &args,
            &mut std::io::stdout().lock(),
            &mut std::io::stderr().lock(),
        )
    };
    std::process::exit(code);
}
