import type {
  LauncherExportOptions,
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
  acknowledgeSteamRunning: boolean;
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
 * Start from what an earlier export stored, so re-exporting never silently moves
 * a work to another prefix.
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
    acknowledgeSteamRunning: false,
  };
}

function selectedInstalls(
  form: LauncherExportForm,
  options: LauncherExportOptions,
) {
  return options.installs.filter((install) => form.selected[install.id]);
}

export function needsSteamAcknowledgement(
  form: LauncherExportForm,
  options: LauncherExportOptions,
) {
  return selectedInstalls(form, options).some(
    (install) => install.launcher === "steam" && install.running,
  );
}

/** Why the form cannot be submitted yet; empty when it can. */
export function launcherExportBlockers(
  form: LauncherExportForm,
  options: LauncherExportOptions,
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
  if (needsSteamAcknowledgement(form, options) && !form.acknowledgeSteamRunning) {
    blockers.push("Close Steam or confirm that you want to export while it runs.");
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
    acknowledgeSteamRunning: form.acknowledgeSteamRunning,
  };
}
