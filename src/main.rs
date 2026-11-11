//! Forge: type an app idea, watch it pass its checks, get a working Rust app.
//!
//! Usage: forge serve | setup [--auto] | eval <app> [--json] | improve | mcp
mod builder;
mod dashboard;
mod evals;
mod improve;
mod mcp;
mod models;
mod release;
mod router;
mod runtime;
mod sandbox;
mod store;
#[cfg(test)]
mod tests;
mod util;

use std::io::IsTerminal;

fn run(args: &[String]) -> util::R<i32> {
    match args.first().map(String::as_str).unwrap_or("serve") {
        "serve" => {
            if !router::local_path().exists() {
                println!("First run: looking for model servers on this computer...");
                if models::setup(false)? {
                    println!("Run `forge setup` any time to choose different models.\n");
                }
            }
            dashboard::serve()?;
        }
        "setup" => {
            let interactive = std::io::stdin().is_terminal() && !args.iter().any(|a| a == "--auto");
            return Ok(if models::setup(interactive)? { 0 } else { 1 });
        }
        "eval" if args.len() > 1 => {
            let card = evals::run_host(&args[1], &util::root())?;
            if args.iter().any(|a| a == "--json") {
                println!("{card}");
            } else {
                println!("{}: {:.0}% of {} cases pass, slowest {} ms, ${:.4} per request", args[1],
                         card["success"].as_f64().unwrap_or(0.0) * 100.0, card["n"], card["p95_ms"], card["usd_per_req"].as_f64().unwrap_or(0.0));
                for f in card["failed"].as_array().into_iter().flatten() {
                    println!("  FAIL {}: {:?}", f["id"].as_str().unwrap_or(""), util::head(f["got"].as_str().unwrap_or(""), 160));
                }
            }
        }
        "improve" => println!("{}", improve::cycle()),
        "mcp" => mcp::serve(),
        _ => {
            println!("Usage: forge serve | setup [--auto] | eval <app> [--json] | improve | mcp");
            return Ok(2);
        }
    }
    Ok(0)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}
