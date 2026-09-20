import { defineConfig } from "vite";
import solid from "vite-plugin-solid";

export default defineConfig({
  plugins: [solid()],
  // Tauri drives this process; its own output matters more than Vite's banner.
  clearScreen: false,
  server: { port: 5173, strictPort: true },
  build: {
    // WebView2 is evergreen Chromium, but Linux ships WebKitGTK - keep the
    // floor at a baseline both satisfy rather than assuming Chromium.
    target: "es2022",
    sourcemap: false,
    // Vite 8 minifies with Oxc natively. Naming "esbuild" here takes a
    // deprecated path that requires esbuild as a separate install.
    minify: true,
    reportCompressedSize: false,
  },
});
