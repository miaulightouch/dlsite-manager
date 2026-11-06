import type {
  LauncherExportOptions,
  LauncherExportReport,
  LauncherExportRequest,
  LauncherInstall,
} from "$lib/model/types";

export const CUSTOM_COMPAT_TOOL = "__custom__";

export type LauncherExportForm = {
  selected: Record<string, boolean>;
  steamUsers: Record<string, string[]>;
  executable: string;
  toolChoice: string;
  customTool: string;
  displayName: string;
  prefix: string;
  acknowledgeRunning: boolean;
  allowPrefixChange: boolean;
};

const launcherNames = {
  steam: "Steam",
  faugus: "Faugus Launcher",
  heroic: "Heroic",
} as const;

export function launcherLabel(install: LauncherInstall) {
  const name = launcherNames[install.launcher];
  return install.flavor === "flatpak" ? `${name} (Flatpak)` : name;
}

function preferredTool(options: LauncherExportOptions) {
  const earlier = options.existing.find((entry) => entry.compatTool)?.compatTool;
  if (earlier) return earlier;
  return options.compatTools.find((tool) => /proton-dl/i.test(tool.id))?.id ?? "";
}

/**
 * Start from what an earlier export stored. Launchers can disagree; the form holds
 * one value, so any selected launcher it would move needs explicit confirmation.
 */
export function initialLauncherExportForm(
  options: LauncherExportOptions,
): LauncherExportForm {
  const listed = (path: string | null | undefined) =>
    options.executables.some((candidate) => candidate.path === path);
  const earlierExecutable = options.existing.find((entry) =>
    listed(entry.executable),
  )?.executable;
  const tool = preferredTool(options);
  const toolListed = options.compatTools.some((known) => known.id === tool);
  const firstTool = options.compatTools[0]?.id;

  const selected: Record<string, boolean> = {};
  const steamUsers: Record<string, string[]> = {};
  for (const install of options.installs) {
    selected[install.id] =
      options.installs.length === 1 ||
      options.existing.some((entry) => entry.installId === install.id);
    if (install.launcher === "steam") {
      steamUsers[install.id] = install.steamUsers.map((user) => user.id);
    }
  }

  return {
    selected,
    steamUsers,
    executable: earlierExecutable ?? options.suggestedExecutable ?? "",
    toolChoice:
      tool && !toolListed ? CUSTOM_COMPAT_TOOL : tool || firstTool || CUSTOM_COMPAT_TOOL,
    customTool: toolListed ? "" : tool,
    displayName:
      options.existing.find((entry) => entry.displayName)?.displayName ??
      options.defaultDisplayName,
    prefix:
      options.existing.find((entry) => entry.prefix)?.prefix ??
      options.defaultPrefix,
    acknowledgeRunning: false,
    allowPrefixChange: false,
  };
}

function selectedInstalls(
  form: LauncherExportForm,
  options: LauncherExportOptions,
) {
  return options.installs.filter((install) => form.selected[install.id]);
}

/**
 * Selected launchers known to be running. The backend's refusal counts too: the
 * snapshot in `options` is older than the export and a launcher may have started.
 */
export function runningLauncherLabels(
  form: LauncherExportForm,
  options: LauncherExportOptions,
  report: LauncherExportReport | null = null,
) {
  const refused = new Set(
    report?.results
      .filter((result) => result.needsAcknowledgement)
      .map((result) => result.installId),
  );
  return selectedInstalls(form, options)
    .filter((install) => install.running || refused.has(install.id))
    .map(launcherLabel);
}

/** What an earlier export of this work stored, for one launcher. */
export function existingExportsFor(
  options: LauncherExportOptions,
  installId: string,
) {
  return options.existing.filter((entry) => entry.installId === installId);
}

/** Selected earlier exports whose prefix differs from the form's. */
export function prefixMoves(
  form: LauncherExportForm,
  options: LauncherExportOptions,
  report: LauncherExportReport | null = null,
) {
  const prefix = form.prefix.trim();
  const refused = new Set(
    report?.results
      .filter((result) => result.needsPrefixChange)
      .map((result) => result.installId),
  );
  const moves = selectedInstalls(form, options).flatMap((install) =>
    existingExportsFor(options, install.id)
      .filter(
        (entry) =>
          entry.prefix !== prefix &&
          (install.launcher !== "steam" ||
            form.steamUsers[install.id]?.includes(entry.steamUserId ?? "")),
      )
      .map((entry) => ({
        label: entry.steamUserId
          ? `${launcherLabel(install)} user ${entry.steamUserId}`
          : launcherLabel(install),
        from: entry.prefix,
      })),
  );
  const listed = new Set(moves.map((move) => move.label));
  for (const install of selectedInstalls(form, options)) {
    if (refused.has(install.id) && ![...listed].some((label) => label.startsWith(launcherLabel(install)))) {
      moves.push({ label: launcherLabel(install), from: null });
    }
  }
  return moves;
}

/** Why the form cannot be submitted yet; empty when it can. */
export function launcherExportBlockers(
  form: LauncherExportForm,
  options: LauncherExportOptions,
  report: LauncherExportReport | null = null,
) {
  const blockers: string[] = [];
  const installs = selectedInstalls(form, options);
  if (!installs.length) blockers.push("Choose at least one launcher.");
  if (
    installs.some(
      (install) =>
        install.launcher === "steam" && !form.steamUsers[install.id]?.length,
    )
  ) {
    blockers.push("Choose at least one Steam user.");
  }
  if (!form.executable) blockers.push("Choose an executable.");
  if (!compatToolValue(form)) blockers.push("Choose a compatibility tool.");
  if (!form.displayName.trim()) blockers.push("Enter a display name.");
  if (!form.prefix.trim().startsWith("/")) {
    blockers.push("Enter an absolute Wine prefix path.");
  }
  if (prefixMoves(form, options, report).length && !form.allowPrefixChange) {
    blockers.push("Keep the current Wine prefix or confirm moving to the new one.");
  }
  if (
    runningLauncherLabels(form, options, report).length &&
    !form.acknowledgeRunning
  ) {
    blockers.push("Close the running launcher or confirm exporting anyway.");
  }
  return blockers;
}

function compatToolValue(form: LauncherExportForm) {
  return form.toolChoice === CUSTOM_COMPAT_TOOL
    ? form.customTool.trim()
    : form.toolChoice;
}

export function buildLauncherExportRequest(
  form: LauncherExportForm,
  options: LauncherExportOptions,
): LauncherExportRequest {
  return {
    executable: form.executable,
    displayName: form.displayName.trim(),
    compatTool: compatToolValue(form),
    prefix: form.prefix.trim(),
    targets: selectedInstalls(form, options).map((install) => ({
      installId: install.id,
      steamUserIds:
        install.launcher === "steam" ? (form.steamUsers[install.id] ?? []) : [],
    })),
    acknowledgeRunning: form.acknowledgeRunning,
    allowPrefixChange: form.allowPrefixChange,
  };
}
