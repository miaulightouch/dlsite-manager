import { describe, expect, it } from "vitest";
import type { LauncherExportOptions } from "$lib/model/types";
import {
  buildLauncherExportRequest,
  CUSTOM_COMPAT_TOOL,
  initialLauncherExportForm,
  launcherExportBlockers,
  launcherLabel,
} from "./launcher-export";

const options: LauncherExportOptions = {
  executables: [
    { path: "/lib/RJ1/Game.exe", relativePath: "Game.exe", sizeBytes: 10 },
    { path: "/lib/RJ1/Other.exe", relativePath: "Other.exe", sizeBytes: 10 },
  ],
  suggestedExecutable: "/lib/RJ1/Game.exe",
  scanTruncated: false,
  defaultDisplayName: "Title",
  defaultPrefix: "/home/u/Games/dlsite-manager/prefixes/RJ1",
  installs: [
    {
      id: "steam:/s",
      launcher: "steam",
      flavor: "native",
      path: "/s",
      steamUsers: [
        { id: "1", name: "A" },
        { id: "2", name: null },
      ],
      running: false,
    },
    {
      id: "heroic:/h",
      launcher: "heroic",
      flavor: "flatpak",
      path: "/h",
      steamUsers: [],
      running: false,
    },
  ],
  compatTools: [
    { id: "proton_experimental", displayName: "Proton - Experimental", path: "/p" },
    { id: "Proton-DL", displayName: "Proton-DL", path: "/d" },
  ],
  existing: [],
  warnings: [],
};

describe("launcher export form", () => {
  it("starts from suggestions, prefers Proton-DL and selects nothing among several launchers", () => {
    const form = initialLauncherExportForm(options);
    expect(form.executable).toBe("/lib/RJ1/Game.exe");
    expect(form.toolChoice).toBe("Proton-DL");
    expect(form.displayName).toBe("Title");
    expect(form.prefix).toBe(options.defaultPrefix);
    expect(form.selected).toEqual({ "steam:/s": false, "heroic:/h": false });
    expect(form.steamUsers["steam:/s"]).toEqual(["1", "2"]);
    expect(launcherExportBlockers(form, options)).toEqual([
      "Choose at least one launcher.",
    ]);
  });

  it("keeps the values of an earlier export, including a changed prefix", () => {
    const form = initialLauncherExportForm({
      ...options,
      existing: [
        {
          installId: "heroic:/h",
          displayName: "Renamed",
          executable: "/lib/RJ1/Other.exe",
          prefix: "/custom/prefix",
          compatTool: "My-Tool",
        },
      ],
    });
    expect(form.prefix).toBe("/custom/prefix");
    expect(form.displayName).toBe("Renamed");
    expect(form.executable).toBe("/lib/RJ1/Other.exe");
    expect(form.toolChoice).toBe(CUSTOM_COMPAT_TOOL);
    expect(form.customTool).toBe("My-Tool");
    expect(form.selected["heroic:/h"]).toBe(true);
    expect(form.selected["steam:/s"]).toBe(false);
  });

  it("ignores an earlier executable that no longer exists and handles no executables", () => {
    const form = initialLauncherExportForm({
      ...options,
      executables: [],
      suggestedExecutable: null,
      existing: [
        {
          installId: "heroic:/h",
          displayName: null,
          executable: "/gone.exe",
          prefix: null,
          compatTool: null,
        },
      ],
    });
    expect(form.executable).toBe("");
    expect(launcherExportBlockers(form, options)).toContain(
      "Choose an executable.",
    );
  });

  it("falls back to free text when no tools are detected", () => {
    const noTools = { ...options, compatTools: [] };
    const form = initialLauncherExportForm(noTools);
    expect(form.toolChoice).toBe(CUSTOM_COMPAT_TOOL);
    form.selected["heroic:/h"] = true;
    expect(launcherExportBlockers(form, noTools)).toEqual([
      "Choose a compatibility tool.",
    ]);
    form.customTool = " proton_9 ";
    expect(launcherExportBlockers(form, noTools)).toEqual([]);
    expect(buildLauncherExportRequest(form, noTools).compatTool).toBe("proton_9");
  });

  it("requires confirmation only when a selected Steam is running", () => {
    const running = {
      ...options,
      installs: options.installs.map((install) =>
        install.launcher === "steam" ? { ...install, running: true } : install,
      ),
    };
    const form = initialLauncherExportForm(running);
    form.selected["heroic:/h"] = true;
    expect(launcherExportBlockers(form, running)).toEqual([]);
    form.selected["steam:/s"] = true;
    expect(launcherExportBlockers(form, running)).toHaveLength(1);
    form.acknowledgeSteamRunning = true;
    expect(launcherExportBlockers(form, running)).toEqual([]);
  });

  it("builds a request with selected targets and Steam users only", () => {
    const form = initialLauncherExportForm(options);
    form.selected["steam:/s"] = true;
    form.steamUsers["steam:/s"] = ["2"];
    form.displayName = "  Name ";
    expect(buildLauncherExportRequest(form, options)).toEqual({
      executable: "/lib/RJ1/Game.exe",
      displayName: "Name",
      compatTool: "Proton-DL",
      prefix: options.defaultPrefix,
      targets: [{ installId: "steam:/s", steamUserIds: ["2"] }],
      acknowledgeSteamRunning: false,
    });
    form.steamUsers["steam:/s"] = [];
    expect(launcherExportBlockers(form, options)).toEqual([
      "Choose at least one Steam user.",
    ]);
  });

  it("labels flatpak installs", () => {
    expect(launcherLabel(options.installs[0])).toBe("Steam");
    expect(launcherLabel(options.installs[1])).toBe("Heroic (Flatpak)");
  });
});
