# Hardware and launcher ownership

This replaces the independently probing AI settings presenter and the synchronous
Home launcher adapter. It does not implement model installation or live speech controls.

## Hardware facts

`ai_runtime::system_hardware::SystemHardware::detect()` owns OS inventory in Rust.
Windows uses DXGI `IDXGIFactory1/IDXGIAdapter1` to enumerate hardware adapters and
dedicated memory. NVIDIA compute capability comes from the CUDA Driver API loaded
from System32. CUDA ordinals are correlated to DXGI adapters by LUID, never array
position. AMD/Intel cards are enumerated without depending on the NVIDIA driver.
Missing CUDA metadata remains missing and is reported, not guessed from GPU names.
Other operating systems currently report explicit unsupported system inventory.

Inventory runs through spawn_blocking in the desktop runtime. HardwarePresenter
projects its typed snapshot; the Slint adapter only maps the view. Refresh uses the
runtime command queue. No nvidia-smi, PowerShell detector, Python interpreter or model
is needed for this OS inventory. No global HardwareApi or separate mutable GPU
registry is introduced: this is a stateless read capability inside Core.

Driver presence and SM do not prove that a Python backend is installed or executable.
The existing AiSupervisor/HardwareRuntime still owns backend inspection, execution
proof and worker activation. An unconfigured desktop AI plan is displayed as
unconfigured, not silently interpreted as shared mode. Maintenance/process control
buttons remain unavailable until the corresponding application commands exist.

The native DXGI COM interfaces are released by RAII. CUDA symbols remain pinned for
the complete query and the driver module reference is released afterward. The query
does not allocate model tensors or select a precision.

## Plugin restrictions

SDK extension: `DynamicConfigurationContract.configuration_constraints(context)`.
Implement it on InstallContract when install/runtime modules are split; otherwise
on Plugin. Context contains effective schema-validated settings, requested runtime
or null, and actual worker hardware. A restriction contains exactly:

| Field | Contract |
| --- | --- |
| field | `settings.<field>` or `runtime.<field>`, one flat field |
| value | A finite JSON scalar |
| severity | `warning` or `forbidden` |
| code | Stable machine-readable reason |
| message | Explanation for display/diagnostics |

Lists are bounded at 256 entries, text at 2048 UTF-8 bytes per field. Unknown fields,
malformed scopes and severities are errors. Inspection never initializes a model.
The host's `configuration_constraints` operation returns the list through the normal
correlated IPC. Absence of the capability is `DynamicConfigurationUnavailable`, not
an empty successful list. AiSupervisor exposes a one-shot inspection operation and
Rust validates the returned contract before publishing it to consumers.

Core owns application-wide recommendation policies. Plugins own restrictions on
their settings/backends; these are not copied into GUI tables. Restrictions compose
as a union: neither a warning nor another provider can relax a forbidden value.
Rust can evaluate them before submission. The host recomputes restrictions against
fresh worker facts and enforces them before calling initialize/initialize_runtime.
Declared schema defaults are checked too, preventing omission from bypassing a ban.
The host never replaces fp16 with fp32 or changes another selected value. Existing
backend execution checks remain authoritative for the actual execution environment.
This SDK contract does not itself add a generic model-settings editor to Slint.

## Launcher

LauncherApi remains the existing static factory/facade. LauncherController owns a
single LauncherSession on a dedicated Core thread. GUI callbacks submit typed commands
to a bounded queue; native start/scan/open/stop/wait work happens on the owner thread.
State and errors are pushed to Slint's event loop. The GUI has no process polling
timer. The owner observes its child internally and reports installation changes.

The application creates the owner and explicitly shuts it down after the UI loop,
including UI-loop failure. Shutdown closes admission, discards queued commands and
cleans up the owned child before joining. Drop closes admission without blocking a
live UI event loop. Other processes are never adopted or terminated.

Home component states use Core connection and detected Unity installation, not fixed
Python reference versions. Its release list comes from ReleasesPresenter; no release
fixtures are injected in ordinary startup. Updates/install/verification actions are
still separate unfinished capabilities, not claimed by the launch/stop integration.

## Verification

Native inventory was run on the host: RTX 4060, measured DXGI memory and CUDA SM 8.9,
with a successful LUID match and no inventory errors. AMD/Intel hardware and non-Windows
inventory have not been physically tested. SDK tests run on Python 3.12.9, including
forbidden/default-value enforcement and pre-initialization rejection. Core tests cover
constraint evaluation and background launcher error delivery/shutdown admission.
No Unity game, model download, paid API or physical audio input is used for these checks.

Results: default suite 505 passed, zero failed, two ignored. Full GUI run had
511 passed and one missing-English-translation failure, corrected afterward;
GUI binding/launcher/voice and localization rerun: 16 passed. Python SDK: 86 passed.
Default/GUI all-target Clippy with warnings denied, fmt and offline GUI binaries
build pass. These are separate from native UI minimize/DPI or real Unity acceptance.
