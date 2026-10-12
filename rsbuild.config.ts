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
    alias: { '@alef-tron/api': './packages/api/src/index.ts' },
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
