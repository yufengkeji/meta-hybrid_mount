## 2025-05-18 - WebUI Cleanup
**Insight:** The `webui` directory (SolidJS) contained `svelte.config.js` and `svelte-check` dependency, which were unused remnants of a previous stack or template.
**Guideline:** Always check project roots for config files that do not match the primary framework (e.g., Svelte files in a Solid/React project) to reduce confusion.
