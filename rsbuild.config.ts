import { defineConfig, type RsbuildPlugin } from '@rsbuild/core';
import { fileURLToPath } from 'node:url';
import { assembleControls } from './frontend/assemble-app.ts';
import { pluginReact } from '@rsbuild/plugin-react';
import { pluginTailwindcss } from '@rsbuild/plugin-tailwindcss';

const assembly: RsbuildPlugin = {
  name: 'alef-app-assembly',
  setup(api) {
    api.onAfterBuild(({ stats }) => {
      if (stats?.hasErrors()) return;
      assembleControls(
        fileURLToPath(new URL('./frontend/app-source/', import.meta.url)),
        fileURLToPath(new URL('./frontend/app/', import.meta.url)),
      );
    });
  },
};

export default defineConfig({
  plugins: [pluginReact(), pluginTailwindcss(), assembly],
  source: {
    entry: { index: './frontend/src/main.tsx' },
    alias: {
      // Sources of the segmented package (API-4); a consumer adds its subpaths here and in tsconfig paths.
      '@alef-runtime/api/types': './packages/api/types/index.ts',
      '@alef-runtime/api/window': './packages/api/src/desktop/window.ts',
      '@alef-runtime/api/app': './packages/api/src/desktop/app.ts',
      '@alef-runtime/api/fs': './packages/api/src/data/fs.ts',
      '@alef-runtime/api/store': './packages/api/src/data/store.ts',
      '@alef-runtime/api/log': './packages/api/src/data/log.ts',
      '@alef-runtime/api/path': './packages/api/src/system/path.ts',
      '@alef-runtime/api/core': './packages/api/src/core/index.ts',
      '@alef-runtime/api': './packages/api/src/index.ts',
    },
  },
  html: {
    template: './frontend/index.html',
  },
  output: {
    distPath: { root: 'frontend/app/dist' },
    overrideBrowserslist: ['Firefox >= 140'],
  },
  server: {
    host: '127.0.0.1',
    port: 3000,
    strictPort: true,
    publicDir: { name: 'frontend/public' },
  },
});
