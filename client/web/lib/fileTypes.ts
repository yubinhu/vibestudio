// File categories supplied by the server and shared display formatting.

export type FileCategory = "markdown" | "code" | "data" | "image" | "text" | "binary";

export function humanSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}
