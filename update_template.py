import re

with open('docs/issues/ISSUE_TEMPLATE.md', 'r') as f:
    content = f.read()

repo_url = "https://github.com/Tollcraft/soroban-cost-profiler/blob/main"

replacements = {
    "> - `PRD.md`": f"> - [`PRD.md`]({repo_url}/PRD.md)",
    "> - `ARCHITECTURE.md`": f"> - [`ARCHITECTURE.md`]({repo_url}/ARCHITECTURE.md)",
    "> - `ARCHITECTURE_ESSENTIALS.md`": f"> - [`ARCHITECTURE_ESSENTIALS.md`]({repo_url}/ARCHITECTURE_ESSENTIALS.md)",
    "> - `ROADMAP.md`": f"> - [`ROADMAP.md`]({repo_url}/ROADMAP.md)",
    "> - `CONTRIBUTING.md`": f"> - [`CONTRIBUTING.md`]({repo_url}/CONTRIBUTING.md)",
    "> - `AGENTS.md` / `CLAUDE.md`": f"> - [`AGENTS.md`]({repo_url}/AGENTS.md) / [`CLAUDE.md`]({repo_url}/CLAUDE.md)"
}

for old, new in replacements.items():
    content = content.replace(old, new)

with open('docs/issues/ISSUE_TEMPLATE.md', 'w') as f:
    f.write(content)

print("Template updated.")
