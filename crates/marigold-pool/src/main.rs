use clap::Parser;

/// Mining pool for Marigold. Not runnable yet; the first runnable version is M9.
#[derive(Parser)]
#[command(name = "marigold-pool", version)]
struct Cli {}

fn main() {
    let _ = Cli::parse();
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    #[test]
    fn the_version_flag_prints_name_and_version() {
        let text = super::Cli::command().render_version();
        assert_eq!(
            text.trim(),
            format!("marigold-pool {}", env!("CARGO_PKG_VERSION"))
        );
    }
}
