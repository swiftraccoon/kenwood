#![doc = include_str!("../README.md")]

use std::error::Error;
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Command;

mod config;
mod lints;
#[cfg(test)]
mod tests;
mod verify;

type PolicyResult<T> = Result<T, Box<dyn Error>>;

#[derive(Debug, Clone, Copy)]
enum Operation {
    Check,
    Ci,
}

fn arguments() -> PolicyResult<(Operation, PathBuf)> {
    let mut args = std::env::args_os().skip(1);
    let operation = match args.next().as_deref().and_then(std::ffi::OsStr::to_str) {
        Some("check") => Operation::Check,
        Some("ci") => Operation::Ci,
        _ => return Err("usage: workspace-policy <check|ci> [--root <directory>]".into()),
    };
    let root = match args.next() {
        None => std::env::current_dir()?,
        Some(flag) if flag == "--root" => {
            PathBuf::from(args.next().ok_or("--root needs a directory")?)
        }
        Some(_) => return Err("expected --root <directory> after the command".into()),
    };
    if args.next().is_some() {
        return Err("unexpected arguments after the root directory".into());
    }
    Ok((operation, root))
}

fn isolated_ci(root: &std::path::Path, policy: &config::Policy) -> PolicyResult<()> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
    for package in &policy.packages {
        for command in [
            &["clippy", "--all-targets", "--", "-D", "warnings"][..],
            &["test"][..],
        ] {
            println!("{}: cargo {}", package.name, command.join(" "));
            let status = Command::new(&cargo)
                .args(command)
                .current_dir(root.join(&package.path))
                .status()?;
            if !status.success() {
                return Err(format!(
                    "{}: cargo {} failed with {status}",
                    package.name,
                    command.join(" ")
                )
                .into());
            }
        }
    }
    Ok(())
}

fn main() -> PolicyResult<()> {
    let (operation, root) = arguments()?;
    let policy = config::Policy::read(&root)?;
    verify::check(&root, &policy)?;
    match operation {
        Operation::Check => println!(
            "policy inventory checked for {} packages",
            policy.packages.len()
        ),
        Operation::Ci => isolated_ci(&root, &policy)?,
    }
    Ok(())
}
