import js from '@eslint/js'
import globals from 'globals'
import reactHooks from 'eslint-plugin-react-hooks'
import reactRefresh from 'eslint-plugin-react-refresh'
import tseslint from 'typescript-eslint'
import { defineConfig, globalIgnores } from 'eslint/config'

export default defineConfig([
  globalIgnores(['dist']),
  {
    files: ['**/*.{ts,tsx}'],
    extends: [
      js.configs.recommended,
      tseslint.configs.recommended,
      reactHooks.configs.flat.recommended,
      reactRefresh.configs.vite,
    ],
    languageOptions: {
      ecmaVersion: 2020,
      globals: globals.browser,
    },
    rules: {
      // Fetch-on-mount effects intentionally set state; surface them as warnings.
      'react-hooks/set-state-in-effect': 'warn',
      // Stale-closure detector — kept at error; intentional omissions get
      // inline suppressions with a reason.
      'react-hooks/exhaustive-deps': 'error',
      // Underscore prefix marks intentionally unused (e.g. destructured
      // props kept for the StepProps signature).
      '@typescript-eslint/no-unused-vars': [
        'error',
        { argsIgnorePattern: '^_', varsIgnorePattern: '^_', caughtErrorsIgnorePattern: '^_' },
      ],
    },
  },
  {
    // Context modules export both provider and hook; only development HMR differs.
    files: ['src/hooks/*.tsx'],
    rules: {
      'react-refresh/only-export-components': 'off',
    },
  },
])
