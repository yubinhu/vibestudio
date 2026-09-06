"use client";

// File-tree icons using VS Code's Material Icon Theme (self-hosted SVGs from the
// `vscode-material-icons` package, copied to /public/material-icons). These are
// the authentic, up-to-date language logos — python, js/ts badges, json, etc.

import { getIconForFilePath, getIconForDirectoryPath } from "vscode-material-icons";

const ICONS_URL = "/material-icons";

function Img({ icon, fallback, size = 16 }: { icon: string; fallback: string; size?: number }) {
  return (
    <img
      src={`${ICONS_URL}/${icon}.svg`}
      alt=""
      width={size}
      height={size}
      draggable={false}
      className="shrink-0"
      onError={(e) => {
        const el = e.currentTarget;
        if (el.dataset.fb !== "1") {
          el.dataset.fb = "1";
          el.src = `${ICONS_URL}/${fallback}.svg`;
        }
      }}
    />
  );
}

export function FileIcon({ name, size = 16 }: { name: string; size?: number }) {
  return <Img icon={getIconForFilePath(name)} fallback="document" size={size} />;
}

/** Folder icon (open / closed) — Material ships an `-open` variant per folder. */
export function FolderIcon({ open, name, size = 16 }: { open: boolean; name: string; size?: number }) {
  const base = getIconForDirectoryPath(name);
  return <Img icon={open ? `${base}-open` : base} fallback={open ? "folder-open" : "folder"} size={size} />;
}
