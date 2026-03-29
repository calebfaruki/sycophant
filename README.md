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
    Sycophant deploys AI agents as Kubernetes pods. Each agent is a group of isolated containers: a workspace where tools run with no network and no credentials, a daemon that proxies LLM and MCP calls over a socket, and a credential proxy for CLI tools. The agent works, but never holds a key, a token, or anything worth stealing.
</p>

## License

[AGPL-3.0](LICENSE)