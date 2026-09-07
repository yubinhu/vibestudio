import js from "@eslint/js";
import globals from "globals";
import reactHooks from "eslint-plugin-react-hooks";
import reactRefresh from "eslint-plugin-react-refresh";
import boundaries from "eslint-plugin-boundaries";
import tseslint from "typescript-eslint";

export default tseslint.config(
  { ignores: ["dist", "build", "client/desktop", "server", "public", "node_modules", ".next"] },
  {
    files: ["**/*.{ts,tsx}"],
    extends: [js.configs.recommended, ...tseslint.configs.recommended],
    languageOptions: {
      ecmaVersion: 2022,
      globals: { ...globals.browser },
    },
    plugins: {
      "react-hooks": reactHooks,
      "react-refresh": reactRefresh,
      boundaries,
    },
    settings: {
      // Resolve the `@/*` alias so boundaries can map imports to layers.
      "import/resolver": { typescript: { project: "./tsconfig.json" } },
      "boundaries/include": ["client/web/**/*"],
      // The architecture's four layers. A page captures its folder name so the
      // rule below can forbid one page importing another's internals.
      "boundaries/elements": [
        { type: "app", mode: "full", pattern: "client/web/app/**" },
        { type: "app", mode: "full", pattern: "client/web/main.tsx" },
        { type: "pages", mode: "full", pattern: "client/web/pages/*/**", capture: ["page", "_"] },
        { type: "components", mode: "full", pattern: "client/web/components/**" },
        { type: "lib", mode: "full", pattern: "client/web/lib/**" },
      ],
    },
    rules: {
      ...reactHooks.configs.recommended.rules,
      "react-refresh/only-export-components": ["warn", { allowConstantExport: true }],
      "no-empty": ["error", { allowEmptyCatch: true }],
      "no-irregular-whitespace": ["error", { skipRegExps: true, skipTemplates: true }],
      // One-directional dependency graph: lib <- components <- pages <- app.
      // A page may use components + lib + its OWN folder, never a sibling page,
      // never the app layer. This is what keeps each page folder independently
      // editable (parallel work) and stops `components`/`lib` coupling upward.
      "boundaries/dependencies": [
        "error",
        {
          default: "disallow",
          message:
            "Layer violation: {{ from.type }} may not import {{ to.type }}. Allowed: lib <- components <- pages <- app; a page may not import a sibling page.",
          rules: [
            { from: [{ type: "lib" }], allow: [{ to: { type: "lib" } }] },
            {
              from: [{ type: "components" }],
              allow: [{ to: { type: "components" } }, { to: { type: "lib" } }],
            },
            {
              from: [{ type: "pages" }],
              allow: [
                { to: { type: "components" } },
                { to: { type: "lib" } },
                { to: { type: "pages", captured: { page: "{{ from.captured.page }}" } } },
              ],
            },
            {
              from: [{ type: "app" }],
              allow: [
                { to: { type: "app" } },
                { to: { type: "pages" } },
                { to: { type: "components" } },
                { to: { type: "lib" } },
              ],
            },
          ],
        },
      ],
    },
  },
);
