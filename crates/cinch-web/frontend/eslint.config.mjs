import { dirname } from "path";
import { fileURLToPath } from "url";
import nextConfig from "eslint-config-next";
import tseslint from "typescript-eslint";

const __dirname = dirname(fileURLToPath(import.meta.url));

export default tseslint.config(
  // ── Ignore patterns ────────────────────────────────────────────────
  {
    ignores: [
      "node_modules/",
      ".next/",
      "out/",
      "next-env.d.ts",
      "eslint.config.mjs",
      "postcss.config.mjs",
    ],
  },

  // ── Next.js recommended (flat config) ──────────────────────────────
  ...nextConfig,

  // ── typescript-eslint strict + stylistic ───────────────────────────
  ...tseslint.configs.strictTypeChecked,
  ...tseslint.configs.stylisticTypeChecked,

  // ── Parser options for type-aware linting ──────────────────────────
  {
    languageOptions: {
      parserOptions: {
        projectService: true,
        tsconfigRootDir: __dirname,
      },
    },
  },

  // ── Custom strict rules ────────────────────────────────────────────
  {
    rules: {
      // Require explicit return types on exported functions.
      "@typescript-eslint/explicit-function-return-type": [
        "error",
        {
          allowExpressions: true,
          allowTypedFunctionExpressions: true,
          allowHigherOrderFunctions: true,
          allowDirectConstAssertionInArrowFunctions: true,
          allowConciseArrowFunctionExpressionsStartingWithVoid: false,
        },
      ],

      // Require explicit accessibility modifiers on class members.
      "@typescript-eslint/explicit-member-accessibility": "error",

      // Ban @ts-ignore, require @ts-expect-error with description.
      "@typescript-eslint/ban-ts-comment": [
        "error",
        {
          "ts-expect-error": "allow-with-description",
          "ts-ignore": true,
          "ts-nocheck": true,
          "ts-check": false,
          minimumDescriptionLength: 10,
        },
      ],

      // Enforce consistent type imports.
      "@typescript-eslint/consistent-type-imports": [
        "error",
        { prefer: "type-imports", fixStyle: "separate-type-imports" },
      ],

      // Enforce consistent type exports.
      "@typescript-eslint/consistent-type-exports": "error",

      // No floating promises — must be awaited or explicitly voided.
      "@typescript-eslint/no-floating-promises": "error",

      // No misused promises (e.g. passing async to void-returning callback).
      "@typescript-eslint/no-misused-promises": [
        "error",
        { checksVoidReturn: { attributes: false } },
      ],

      // Require Promise-like values to be handled appropriately.
      "@typescript-eslint/require-await": "error",

      // Disallow unnecessary type assertions.
      "@typescript-eslint/no-unnecessary-type-assertion": "error",

      // Enforce using nullish coalescing over logical OR for nullable values.
      "@typescript-eslint/prefer-nullish-coalescing": "error",

      // Enforce using optional chaining.
      "@typescript-eslint/prefer-optional-chain": "error",

      // Enforce switch exhaustiveness (with union types).
      "@typescript-eslint/switch-exhaustiveness-check": "error",

      // No unused vars (error, not warning).
      "@typescript-eslint/no-unused-vars": [
        "error",
        { argsIgnorePattern: "^_", varsIgnorePattern: "^_" },
      ],

      // Enforce using Array<T> consistently.
      "@typescript-eslint/array-type": ["error", { default: "array-simple" }],

      // Enforce type-only imports where possible.
      "@typescript-eslint/no-import-type-side-effects": "error",

      // General strict rules.
      "no-console": ["error", { allow: ["warn", "error"] }],
      eqeqeq: ["error", "always"],
      "no-implicit-coercion": "error",
      "prefer-const": "error",
      "no-var": "error",
      "object-shorthand": "error",
      curly: ["error", "multi-line"],
    },
  },
);
