<p align="center">
  <img src="docs/logo.png" alt="Sycophant" />
</p>

<h2 align="center">
  Secure-by-default agent framework for vibe coders and DevOps teams
</h2>

<h3 align="center">
  Logo by <a href="https://www.yo-bullitt.com/">Bullitt</a>
</h3>

<p align="center">
  <a href="https://scorecard.dev/viewer/?uri=github.com/calebfaruki/sycophant"><img src="https://api.scorecard.dev/projects/github.com/calebfaruki/sycophant/badge" alt="OpenSSF Scorecard"></a>
  <a href="https://www.rust-lang.org/"><img src="https://img.shields.io/badge/Made%20with-Rust-1f425f.svg" alt="Made with Rust"></a>
</p>

<p align="center">
    Sycophant deploys AI agents on Kubernetes. Each workspace pod runs a transponder (message router) and workspace-tools (local MCP server) with no network egress and no mounted secrets. A shared tightbeam controller proxies LLM calls via ephemeral Jobs. A shared airlock controller executes tools in isolated chambers with scoped credentials and network egress. Secrets are projected only into ephemeral Jobs — the long-lived pods never mount them.
</p>

## License

[AGPL-3.0](LICENSE)