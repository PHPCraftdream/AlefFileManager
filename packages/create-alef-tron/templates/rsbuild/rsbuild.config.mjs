import { defineConfig } from '@rsbuild/core';

export default defineConfig({
  source: { entry: { index: './src/main.js' } },
  html: { template: './index.html' },
  output: {
    distPath: { root: 'dist' },
    assetPrefix: './',
  },
  dev: { assetPrefix: './' },
  server: { host: '127.0.0.1', port: 3000, strictPort: true },
});
