---
name: dsg-disk-health
description: Safety-first DSG workflow for disk-health in AI coding sessions.
---

# DSG disk-health Skill

1. Start with read-only inventory: `python3 -m dsg doctor --root .` and `python3 -m dsg scan --root .` (run from poc folder or install PYTHONPATH).
2. Check the project ownership and active build/test/server status. Do not assume a Docker image tag or directory name is sufficient identity.
3. Prefer existing resources and incremental builds where evidence supports reuse.
4. Before GC use `python3 -m dsg gc-plan --root <project-path>`. Never apply a plan automatically.
5. Do not delete database volumes, user data, unknown files, active resources, or shared caches.
6. If command risk is detected, provide a non-destructive alternative; do not evade policy.

Current POC is advisory and does not perform system-wide enforcement. Real-world provider features are in the project roadmap.
