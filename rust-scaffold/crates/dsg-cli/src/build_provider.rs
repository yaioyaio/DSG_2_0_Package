use super::CliError;

pub(super) struct ProviderCommand<'a> {
    pub provider: &'a str,
    pub task: &'a str,
    pub argv: Vec<String>,
    pub env: &'static [(&'static str, &'static str)],
}

pub(super) fn prepare<'a>(
    provider: &'a str,
    tasks: &'a [String],
) -> Result<ProviderCommand<'a>, CliError> {
    let [task] = tasks else {
        return Err(invalid_task());
    };
    let task = task.as_str();
    let (argv, env): (Vec<&str>, &[(&str, &str)]) = match (provider, task) {
        ("cargo", "build" | "check" | "test") => {
            let mut args = vec!["cargo", task, "--offline", "--jobs", "1"];
            if task == "test" {
                args.extend(["--", "--test-threads=1"]);
            }
            (args, &[])
        }
        ("node", "test") => (vec!["node", "--test", "--test-concurrency=1"], &[]),
        // One project script at a time. The script owns its internal concurrency.
        ("pnpm", "build" | "test") => (
            vec!["pnpm", "run", task],
            &[
                ("COREPACK_ENABLE_NETWORK", "0"),
                ("npm_config_offline", "true"),
                ("npm_config_workspace_concurrency", "1"),
                ("npm_config_manage_package_manager_versions", "false"),
                ("NEXT_TELEMETRY_DISABLED", "1"),
            ],
        ),
        ("gradle", "build" | "test") => (
            vec!["gradle", "--offline", "--no-daemon", "--max-workers=1",
                 "-Dorg.gradle.jvmargs=-Xmx256m -XX:MaxMetaspaceSize=192m -XX:ActiveProcessorCount=1", task],
            &[],
        ),
        ("uv", "test") => (
            vec!["uv", "run", "--offline", "--no-sync", "--no-python-downloads", "python", "-m", "unittest", "discover"],
            &[],
        ),
        ("go", "build" | "test") => {
            let mut args = vec!["go", task, "-p", "1"];
            if task == "test" {
                args.extend(["-parallel", "1"]);
            }
            args.push("./...");
            (args, &[("GOMAXPROCS", "1"), ("GOPROXY", "off"), ("GOTOOLCHAIN", "local"), ("GONOPROXY", "none"), ("GOVCS", "*:off")])
        }
        ("playwright", "test") => (
            vec!["./node_modules/.bin/playwright", "test", "--workers=1", "--retries=0", "--trace=on"],
            &[("PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD", "1")],
        ),
        _ => return Err(invalid_task()),
    };
    Ok(ProviderCommand {
        provider,
        task,
        argv: argv.into_iter().map(String::from).collect(),
        env,
    })
}

fn invalid_task() -> CliError {
    CliError::new(
        "PROVIDER_TASK_INVALID",
        "등록된 Provider와 해당 작업 이름 하나를 지정하세요",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_registered_tasks_are_accepted() {
        for (name, task, program) in [
            ("cargo", "check", "cargo"),
            ("node", "test", "node"),
            ("pnpm", "build", "pnpm"),
            ("gradle", "test", "gradle"),
            ("uv", "test", "uv"),
            ("go", "test", "go"),
            ("playwright", "test", "./node_modules/.bin/playwright"),
        ] {
            let input = [task.to_owned()];
            let command =
                prepare(name, &input).unwrap_or_else(|_| panic!("registered provider {name}"));
            assert_eq!(command.argv[0], program);
            assert_eq!(command.task, task);
            assert!(prepare(name, &[]).is_err());
            assert!(prepare(name, &[task.into(), "--workers=100".into()]).is_err());
        }
        assert!(prepare("cargo", &["install".into()]).is_err());
        assert!(prepare("other", &["test".into()]).is_err());
    }
}
