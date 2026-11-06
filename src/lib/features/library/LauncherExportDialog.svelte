<script lang="ts">
  import { untrack } from "svelte";
  import Button from "$lib/components/workspace/Button.svelte";
  import Modal from "$lib/components/workspace/Modal.svelte";
  import type {
    LauncherExportOptions,
    LauncherExportReport,
    LauncherExportRequest,
  } from "$lib/model/types";
  import { formatBytes } from "$lib/utils/format";
  import {
    buildLauncherExportRequest,
    CUSTOM_COMPAT_TOOL,
    existingExportsFor,
    initialLauncherExportForm,
    launcherExportBlockers,
    launcherLabel,
    prefixMoves,
    runningLauncherLabels,
    type LauncherExportForm,
  } from "$lib/utils/launcher-export";

  let {
    workId,
    options,
    loadError = null,
    exporting = false,
    exportError = null,
    report = null,
    onExport,
    onClose,
  }: {
    workId: string;
    options: LauncherExportOptions | null;
    loadError?: string | null;
    exporting?: boolean;
    exportError?: string | null;
    report?: LauncherExportReport | null;
    onExport: (request: LauncherExportRequest) => void;
    onClose: () => void;
  } = $props();

  let form = $state<LauncherExportForm | null>(null);
  $effect(() => {
    const loaded = options;
    untrack(() => {
      if (loaded && !form) form = initialLauncherExportForm(loaded);
    });
  });

  const blockers = $derived(
    form && options ? launcherExportBlockers(form, options, report) : [],
  );
  const runningLabels = $derived(
    form && options ? runningLauncherLabels(form, options, report) : [],
  );
  const moves = $derived(
    form && options ? prefixMoves(form, options, report) : [],
  );
  const statusLabels = {
    created: "Added",
    updated: "Updated",
    unchanged: "Already up to date",
    failed: "Failed",
  };

  function installName(installId: string) {
    const install = options?.installs.find((known) => known.id === installId);
    return install ? launcherLabel(install) : installId;
  }

  function toggleSteamUser(installId: string, userId: string, on: boolean) {
    if (!form) return;
    const users = form.steamUsers[installId] ?? [];
    form.steamUsers[installId] = on
      ? [...users, userId]
      : users.filter((id) => id !== userId);
  }

  function submit(event: SubmitEvent) {
    event.preventDefault();
    if (form && options && !blockers.length && !exporting) {
      onExport(buildLauncherExportRequest($state.snapshot(form), options));
    }
  }
</script>

<Modal title={`Export ${workId} to launchers`} wide {onClose}>
  <div class="export">
    {#if loadError}
      <p class="error" role="alert">{loadError}</p>
    {:else if !options || !form}
      <p role="status">Looking for launchers and executables…</p>
    {:else}
      <form onsubmit={submit}>
        {#each options.warnings as warning}
          <p class="warning" role="status">{warning}</p>
        {/each}

        <fieldset>
          <legend>Launchers</legend>
          {#if options.installs.length === 0}
            <p>
              No Steam, Faugus Launcher or Heroic data was found. Start the
              launcher once, then try again.
            </p>
          {/if}
          {#each options.installs as install}
            <label class="check">
              <input type="checkbox" bind:checked={form.selected[install.id]} />
              <span>
                {launcherLabel(install)}
                <small>{install.path}</small>
                {#each existingExportsFor(options, install.id) as entry}
                  <small>
                    Exported{entry.steamUserId
                      ? ` for user ${entry.steamUserId}`
                      : ""} as “{entry.displayName ?? "unnamed"}”, prefix {entry.prefix ??
                      "chosen by the launcher"}
                  </small>
                {/each}
              </span>
            </label>
            {#if install.launcher === "steam" && form.selected[install.id]}
              <div class="nested">
                {#if install.steamUsers.length === 0}
                  <p>No Steam users were found in this install.</p>
                {/if}
                {#each install.steamUsers as user}
                  <label class="check">
                    <input
                      type="checkbox"
                      checked={form.steamUsers[install.id]?.includes(user.id)}
                      onchange={(event) =>
                        toggleSteamUser(
                          install.id,
                          user.id,
                          event.currentTarget.checked,
                        )}
                    />
                    <span>{user.name ? `${user.name} (${user.id})` : user.id}</span>
                  </label>
                {/each}
              </div>
            {/if}
          {/each}
          {#if runningLabels.length}
            <div class="warning" role="alert">
              <p>
                {runningLabels.join(", ")} running. A running launcher can overwrite
                or drop this export: Steam when it exits, Faugus Launcher on its next
                save, Heroic when it next saves the game's settings. Close it first if
                you can.
              </p>
              <label class="check">
                <input type="checkbox" bind:checked={form.acknowledgeRunning} />
                <span>Export while it is running</span>
              </label>
            </div>
          {/if}
        </fieldset>

        <label class="field">
          <span>Game executable</span>
          {#if options.executables.length}
            <select class="dm:draft-focus-field" bind:value={form.executable}>
              {#if !options.suggestedExecutable}
                <option value="">Choose an executable</option>
              {/if}
              {#each options.executables as candidate}
                <option value={candidate.path}>
                  {candidate.relativePath} ({formatBytes(candidate.sizeBytes)})
                </option>
              {/each}
            </select>
            {#if options.scanTruncated}
              <small>The folder is very large; some files were not scanned.</small>
            {/if}
          {:else}
            <p class="warning" role="status">
              No .exe file was found in this work's folder, so it cannot be
              exported. Works kept as archives need to be unpacked first.
            </p>
          {/if}
        </label>

        <label class="field">
          <span>Compatibility tool</span>
          <select class="dm:draft-focus-field" bind:value={form.toolChoice}>
            {#each options.compatTools as tool}
              <option value={tool.id}>{tool.displayName}</option>
            {/each}
            <option value={CUSTOM_COMPAT_TOOL}>Other…</option>
          </select>
          {#if form.toolChoice === CUSTOM_COMPAT_TOOL}
            <input
              class="dm:draft-focus-field"
              aria-label="Compatibility tool name or folder"
              placeholder="Steam tool name or absolute tool folder"
              bind:value={form.customTool}
            />
            <small>
              Heroic and Faugus Launcher need an installed tool folder; Steam
              uses the tool's internal name.
            </small>
          {/if}
        </label>

        <label class="field">
          <span>Display name</span>
          <input class="dm:draft-focus-field" bind:value={form.displayName} />
        </label>

        <label class="field">
          <span>Wine prefix</span>
          <input class="dm:draft-focus-field" bind:value={form.prefix} />
          <small>
            Saves and settings for this work live here. It stays in place when
            you change it or remove the work.
          </small>
        </label>

        {#if moves.length}
          <div class="warning" role="alert">
            <p>This would move earlier exports to the Wine prefix above:</p>
            {#each moves as move}
              <p>{move.label}: from {move.from ?? "its current prefix"}</p>
            {/each}
            <p>Saves stay in the old prefix and are not copied.</p>
            <label class="check">
              <input type="checkbox" bind:checked={form.allowPrefixChange} />
              <span>Move them to the new prefix</span>
            </label>
          </div>
        {/if}

        {#if exportError}
          <p class="error" role="alert">{exportError}</p>
        {/if}

        {#if report}
          <section aria-label="Export results" aria-live="polite">
            {#each report.results as result}
              <div class="result" class:failed={result.status === "failed"}>
                <strong>
                  {installName(result.installId)}: {statusLabels[result.status]}
                </strong>
                {#if result.error}<p class="error">{result.error}</p>{/if}
                {#each result.warnings as warning}<p>{warning}</p>{/each}
                {#each result.files.filter((file) => file.backup) as file}
                  <small>Backup: {file.backup}</small>
                {/each}
              </div>
            {/each}
          </section>
        {/if}

        <div class="actions">
          {#if blockers.length}<small>{blockers[0]}</small>{/if}
          <Button variant="secondary" onclick={onClose}>
            {report ? "Done" : "Cancel"}
          </Button>
          <Button
            variant="primary"
            type="submit"
            disabled={blockers.length > 0 || exporting}
          >
            {exporting ? "Exporting…" : "Export"}
          </Button>
        </div>
      </form>
    {/if}
  </div>
</Modal>

<style>
  .export {
    color: var(--dm-color-draft-ink);
    font-size: 13px;
  }
  form {
    display: grid;
    gap: 16px;
  }
  fieldset {
    display: grid;
    gap: 8px;
    margin: 0;
    padding: 0;
    border: 0;
  }
  legend,
  .field > span {
    padding: 0;
    margin-bottom: 2px;
    font-weight: 600;
  }
  .field {
    display: grid;
    gap: 6px;
  }
  .check {
    display: flex;
    align-items: flex-start;
    gap: 8px;
    cursor: pointer;
  }
  .check input {
    margin: 2px 0 0;
  }
  .check span {
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .check small {
    display: block;
  }
  .nested {
    display: grid;
    gap: 6px;
    margin-left: 24px;
  }
  select,
  input:not([type="checkbox"]) {
    box-sizing: border-box;
    width: 100%;
    min-width: 0;
    padding: 8px 10px;
    border: 1px solid var(--draft-field-border, var(--dm-color-draft-line-strong));
    border-radius: 6px;
    background: var(--dm-color-draft-input);
    color: var(--dm-color-draft-ink);
    font: inherit;
  }
  small,
  p {
    margin: 0;
    color: var(--dm-color-draft-dim);
    font-size: 12px;
    line-height: 1.6;
    overflow-wrap: anywhere;
  }
  .warning {
    display: grid;
    gap: 8px;
    padding: 10px 12px;
    border-radius: 6px;
    background: var(--dm-color-draft-elevated);
    color: var(--dm-color-draft-ink);
  }
  .warning p {
    color: inherit;
  }
  p.error {
    color: var(--dm-color-draft-error);
  }
  section {
    display: grid;
    gap: 8px;
  }
  .result {
    display: grid;
    gap: 4px;
    padding: 10px 12px;
    border: 1px solid var(--dm-color-draft-line);
    border-radius: 6px;
  }
  .result.failed {
    background: var(--dm-color-draft-error-bg);
  }
  .actions {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: flex-end;
    gap: 8px;
  }
  .actions small {
    flex: 1 1 200px;
  }
</style>
