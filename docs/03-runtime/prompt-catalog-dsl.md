# Prompt Catalog and request-scoped DSL (2026-11-05)

## Filesystem convention

The desktop runtime reads `NEUROMITA_PROMPTS_DIR` when set, otherwise
`<data_directory>/Prompts`. The catalog is always attached; missing directories yield an empty list,
and the existing character prompt remains active until a set is installed.
An explicitly selected missing set always fails the request. Each character ID owns `<Prompts>/<CharacterId>/<Set>/`, for example
`Prompts/Crazy/Default/`. `main_template.txt` is required to list a set;
`react_template.txt` is required when using that set for React operations.
`config.json` can contain a Python-compatible flat map of variable defaults or
an object with `variables`; `info.json` contains optional author, version and
description. Runtime character state takes precedence over configured variable
defaults during a request. Paths are canonicalized, bounded to the prompts
root, and cycle/depth/file-size checked.

The default selection policy is intentional UI/UX: when no set is saved,
the catalog chooses a case-insensitive `Default`, otherwise the first set in
the sorted catalog. The selector displays that effective choice. This policy
is retained following the user's clarification; an explicitly selected missing
set is never replaced with another one.
An explicitly selected set also fails before provider invocation when an
application was started without a PromptCatalog; it cannot silently use the
built-in character prompt instead.

## Ownership and lifecycle

`PromptCatalog` owns file discovery and a **new Interpreter per request**.
`SettingsOwner` persists optional `prompt_set` per character; the Characters
Presenter provides a typed binding and GUI options come from the Core catalog.
`MessageHandler` resolves the selected prompt, evaluates DSL with character
context and forwards only the rendered text to the existing generation path.
A failed render terminates the request before provider invocation. No
second character state, event bus or hidden service locator is introduced.

The render result preserves two groups, `stable` and `context`. Current
provider composition joins these into the existing `CharacterContext.prompt`
because the generation adapter does not yet expose separate cacheable and
volatile system-message slots.

## DSL support and remaining Python parity gaps

Implemented: main-template static and volatile includes; nested `.txt`, `.script`
and `.system`; `[<{path}>]`-style includes; `[{name}]` substitution;
`support_intents`, `behavior_state`; `IF/ELSEIF/ELSE/ENDIF`, `SET`, `SET LOCAL`,
`RUN`, `RETURN`, `ADD_SYSTEM_INFO`, `ADD_CONTEXT_INFO`, tagged/inline `LOAD`,
`LOAD_REL`, safe expressions (including f-strings, `round(x, digits)`), and
case-insensitive Boolean conditions. A batch test against the available Python
`extra/Prompts` reference successfully rendered existing `main_template.txt`
and `react_template.txt` files from all discovered sets.

Initialization directives `SEED_MEMORY`, `SEED_RAG_MEMORY`, `SEED_ISLAND`
and `LINK_ENTITIES` are parsed into typed `PromptInitializationDeclaration`
values. **They are not yet persisted to Timeline**: the existing Core has no
RAG-only memory or graph-relation write contract matching Python. Treating
them as arbitrary side effects during request preparation would violate
Timeline ownership. Integrating the declarations into an idempotent, atomic
character-owned initialization operation remains required. The current DSL
also does not implement Python PostDSL or guaranteed full Python expression
semantics. `SET LOCAL` is accepted but does not yet create an isolated
per-script scope. Unknown untrusted expressions fail explicitly.

The original Python reference is not copied into this repository. Audit
results are local verification, not a substitute for a comprehensive parity
fixture suite.
