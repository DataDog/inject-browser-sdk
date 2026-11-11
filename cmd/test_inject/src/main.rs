use std::{env, fs, io::Write as _, process};

use inject_browser_sdk::{
    generate_snippet,
    injector::{InjectionStrategy, Injector, Result as InjectorResult},
    Configuration, RumConfiguration,
};

const STRATEGIES: [(&str, InjectionStrategy); 2] = [
    ("head-close-tag", InjectionStrategy::HeadCloseTag),
    ("head-open-tag", InjectionStrategy::HeadOpenTag),
];

fn names<T>(options: &[(&'static str, T)]) -> String {
    return options
        .iter()
        .map(|(name, _)| return *name)
        .collect::<Vec<_>>()
        .join(", ");
}

fn usage() -> ! {
    println!(
        "Usage: {} <input_file> [strategy]\n\nstrategy is one of {} (default: {}).",
        env::args().next().unwrap(),
        names(&STRATEGIES),
        STRATEGIES[0].0,
    );
    process::exit(1);
}

fn main() {
    let Some(input_file_path) = env::args().nth(1) else {
        usage();
    };

    let strategy = match env::args().nth(2) {
        None => InjectionStrategy::default(),
        Some(name) => match STRATEGIES.iter().find(|(n, _)| return *n == name) {
            Some((_, strategy)) => *strategy,
            None => {
                println!("Unknown strategy: {}", name);
                usage();
            }
        },
    };

    let input_file_content = match fs::read(&input_file_path) {
        Ok(content) => content,
        Err(err) => {
            println!("Error reading file {}: {}", input_file_path, err);
            process::exit(1);
        }
    };

    let configuration = Configuration {
        major_version: 6,
        rum: RumConfiguration {
            application_id: Some("xxx".into()),
            client_token: Some("xxx".into()),
            ..Default::default()
        },
    };

    let snippet = match generate_snippet(&configuration) {
        Ok(snippet) => snippet,
        Err(err) => {
            println!("Error generating snippet: {}", err);
            process::exit(1);
        }
    };

    let mut injector = Injector::with_strategy(&snippet, strategy);
    print_injector_result(injector.write(&input_file_content));
    print_injector_result(injector.end());
}

fn print_injector_result(result: InjectorResult) {
    for slice in result.slices[..result.length].iter() {
        std::io::stdout().write_all(slice.bytes).unwrap();
    }
}
