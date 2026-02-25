use clap::{Parser, Subcommand};
use xshell::{cmd, Shell};

#[derive(Parser)]
#[command(name = "xtask")]
#[command(about = "Automation tasks", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    CrossBuildAll,
    ListSupportedTargets,
    BuildThinFFI {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    BuildWasmFfi {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    TestWasm,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let sh = Shell::new()?;
    let targets = [
        "x86_64-unknown-linux-gnu",
        "x86_64-unknown-linux-musl",
        "aarch64-unknown-linux-gnu",
        "aarch64-unknown-linux-musl",
        "wasm32-unknown-unknown",
    ];

    match cli.command {
        Commands::ListSupportedTargets => {
            for target in targets {
                println!("{}", target);
            }
        }
        Commands::CrossBuildAll => {
            for target in targets {
                println!("Building for target: {}", target);
                if target == "wasm32-unknown-unknown" {
                    cmd!(sh, "cargo build --target {target}").run()?;
                } else {
                    cmd!(sh, "cross build --target {target}").run()?;
                }
            }
        }
        Commands::BuildThinFFI { args } => {
            println!("Building thin FFI");
            cmd!(
                sh,
                "cross build --no-default-features -p inject-browser-sdk-ffi {args...}"
            )
            .run()?;
        }
        Commands::BuildWasmFfi { args } => {
            println!("Building WASM FFI");
            cmd!(sh, "cargo build --target wasm32-unknown-unknown --no-default-features -p inject-browser-sdk-ffi {args...}").run()?;
        }
        Commands::TestWasm => {
            println!("Building WASM FFI (release)...");
            cmd!(sh, "cargo build --target wasm32-unknown-unknown --no-default-features -p inject-browser-sdk-ffi --release").run()?;
            println!("Running Node.js integration tests...");
            cmd!(sh, "node --test wasm/node/test_integration.js").run()?;
        }
    }

    Ok(())
}
