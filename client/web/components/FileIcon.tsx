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

/** VibeStudio brand mark — monochrome via
 *  currentColor so it adapts to light/dark (color it with `text-brand`). Used in
 *  the app chrome (nav bar). */
export function VibeStudioMark({ className }: { className?: string }) {
  return (
    <svg
      viewBox="0 0 1052 1197"
      fill="currentColor"
      aria-hidden
      className={className ?? "h-4.5 w-auto"}
    >
      <path d="M890.534 772.972L859.164 827.387L564.655 656.935L562.117 655.513L559.528 656.986C539.377 668.813 512.931 668.914 492.373 656.986L489.835 655.564L192.788 827.488L161.418 773.073L455.826 602.723L458.364 601.302V598.358C458.465 574.399 471.358 552.014 492.068 539.882L494.606 538.41V194.664H557.295V538.461L559.833 539.882C580.543 552.065 593.487 574.399 593.487 598.358V601.302L890.483 773.073L890.534 772.972Z" />
      <path d="M1018.45 805.459L1015.96 803.987V392.933L1018.45 391.461C1039.41 379.075 1052 357.249 1052 332.985C1052 295.677 1021.65 265.272 984.388 265.272C972.713 265.272 961.19 268.368 951.039 274.205L948.5 275.677L945.963 274.205L593.638 70.3025V67.4092C593.435 30.2529 563.08 0 526.025 0C488.971 0 458.667 30.2529 458.464 67.3584V70.3025L455.926 71.7238L103.55 275.677L101.012 274.256C90.7588 268.419 79.1856 265.272 67.6123 265.272C30.3545 265.272 0 295.677 0 332.985C0 356.893 12.8423 379.329 33.5523 391.461L36.0903 392.933V803.987L33.6031 805.459C12.5885 817.844 0 839.671 0 863.934C0 901.243 30.3545 931.648 67.6123 931.648C79.3886 931.648 91.0634 928.501 101.266 922.562L103.804 921.09L106.342 922.562L458.362 1126.31V1129.21C458.616 1166.36 488.92 1196.56 525.975 1196.56C563.029 1196.56 593.333 1166.36 593.536 1129.21V1126.31L596.074 1124.84L948.196 921.09L950.734 922.562C960.937 928.45 972.611 931.648 984.388 931.648C1021.65 931.648 1052 901.243 1052 863.934C1052 840.026 1039.16 817.59 1018.45 805.459ZM953.323 803.581L950.835 805.053C930.024 817.083 917.029 839.468 916.978 863.528V866.422L914.44 867.893L611.86 1043.07L737.542 824.849L683.229 793.378L526.025 1066.26L368.974 793.53L314.712 824.9L440.393 1043.12L135.377 866.523V863.63C135.275 839.62 122.331 817.235 101.52 805.154L98.9819 803.682V450.19L224.664 668.306L278.926 636.885L121.824 364.152H435.926V301.26H184.563L489.681 124.717L492.27 126.138C502.574 132.128 514.249 135.326 526.076 135.326C537.903 135.326 549.324 132.28 559.933 126.138L562.471 124.717L564.958 126.138L867.589 301.26H616.226V364.152H930.328L773.277 636.885L827.539 668.306L953.221 450.038V803.581H953.323Z" />
    </svg>
  );
}
