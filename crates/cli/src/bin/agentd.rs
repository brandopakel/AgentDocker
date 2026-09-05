//! The daemon binary, shipped with the CLI so one install gets both.

fn main() -> anyhow::Result<()> {
    agentd::main()
}
