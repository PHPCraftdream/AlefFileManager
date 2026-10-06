import { defineConfig } from '@rsbuild/core';
import { pluginReact } from '@rsbuild/plugin-react';
import { pluginTailwindcss } from '@rsbuild/plugin-tailwindcss';

export default defineConfig({
  plugins: [pluginReact(), pluginTailwindcss()],
  source: {
    entry: { index: './frontend/src/main.tsx' },
    alias: { '@alef-tron/api': './packages/api/src/index.ts' },
  },
  html: {
    template: './frontend/index.html',
  },
  output: {
    distPath: { root: 'frontend/dist' },
    overrideBrowserslist: ['Firefox >= 140'],
  },
  server: {
    host: '127.0.0.1',
    port: 3000,
    strictPort: true,
    publicDir: { name: 'frontend/public' },
  },
});
