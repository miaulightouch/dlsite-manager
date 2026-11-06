import type { Meta, StoryObj } from "@storybook/sveltekit";
import { fn } from "storybook/test";
import LauncherExportDialog from "$lib/features/library/LauncherExportDialog.svelte";
import type { LauncherExportOptions } from "$lib/model/types";

const options: LauncherExportOptions = {
  executables: [
    { path: "/library/RJ01553954/Game.exe", relativePath: "Game.exe", sizeBytes: 12_400_000 },
    { path: "/library/RJ01553954/unins000.exe", relativePath: "unins000.exe", sizeBytes: 3_100_000 },
  ],
  suggestedExecutable: "/library/RJ01553954/Game.exe",
  scanTruncated: false,
  defaultDisplayName: "A Long Evening at the Observatory",
  defaultPrefix: "/home/user/Games/dlsite-manager/prefixes/RJ01553954",
  installs: [
    {
      id: "steam:/home/user/.local/share/Steam",
      launcher: "steam",
      flavor: "native",
      path: "/home/user/.local/share/Steam",
      steamUsers: [
        { id: "1234", name: "Player One" },
        { id: "5678", name: null },
      ],
      running: true,
    },
    {
      id: "faugus:/home/user/.local/share/faugus-launcher",
      launcher: "faugus",
      flavor: "native",
      path: "/home/user/.local/share/faugus-launcher",
      steamUsers: [],
      running: false,
    },
    {
      id: "heroic:/home/user/.var/app/com.heroicgameslauncher.hgl/config/heroic",
      launcher: "heroic",
      flavor: "flatpak",
      path: "/home/user/.var/app/com.heroicgameslauncher.hgl/config/heroic",
      steamUsers: [],
      running: false,
    },
  ],
  compatTools: [
    { id: "Proton-DL", displayName: "Proton-DL", path: "/tools/proton-dl" },
    { id: "proton_experimental", displayName: "Proton - Experimental", path: "/tools/exp" },
  ],
  existing: [],
  warnings: [],
};

const meta = {
  title: "Library/Launcher Export Dialog",
  component: LauncherExportDialog,
  parameters: { layout: "fullscreen" },
  args: { workId: "RJ01553954", options, onExport: fn(), onClose: fn() },
} satisfies Meta<typeof LauncherExportDialog>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Default: Story = {};

export const Loading: Story = { args: { options: null } };

export const NoExecutable: Story = {
  args: {
    options: { ...options, executables: [], suggestedExecutable: null },
  },
};

export const NothingDetected: Story = {
  args: { options: { ...options, installs: [], compatTools: [] } },
};

export const PrefixDisagreement: Story = {
  args: {
    options: {
      ...options,
      existing: [
        {
          installId: options.installs[1].id,
          steamUserId: null,
          displayName: "Observatory",
          executable: options.executables[0].path,
          prefix: "/home/user/Faugus/observatory",
          compatTool: "Proton-DL",
        },
        {
          installId: options.installs[2].id,
          steamUserId: null,
          displayName: "A Long Evening",
          executable: options.executables[0].path,
          prefix: options.defaultPrefix,
          compatTool: "Proton-DL",
        },
      ],
    },
  },
};

export const WithResults: Story = {
  args: {
    report: {
      prefix: options.defaultPrefix,
      results: [
        {
          installId: options.installs[1].id,
          launcher: "faugus",
          status: "updated",
          warnings: ["Restart Faugus Launcher to see the change"],
          error: null,
          needsAcknowledgement: false,
          needsPrefixChange: false,
          files: [
            {
              path: "/home/user/.local/share/faugus-launcher/games.json",
              action: "updated",
              backup:
                "/home/user/.local/share/faugus-launcher/games.json.dlsite-manager-20261107T120000Z.bak",
            },
          ],
        },
        {
          installId: options.installs[2].id,
          launcher: "heroic",
          status: "failed",
          warnings: [],
          error: "library.json: invalid JSON; the file was not changed",
          needsAcknowledgement: false,
          needsPrefixChange: false,
          files: [],
        },
      ],
    },
  },
};
