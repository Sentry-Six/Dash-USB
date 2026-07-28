import path from "path"
import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import tailwindcss from '@tailwindcss/vite'

export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "./src"),
    },
  },
  build: {
    rollupOptions: {
      output: {
        // Named vendor chunks so an OTA update that only changes app
        // code doesn't bust the cache for libraries that haven't moved.
        // Each library lives in its own content-hashed file. Standard
        // Rollup `manualChunks` function form. The prior `codeSplitting`
        // key is a rolldown-vite-only API, but this build runs plain
        // vite@8, so it failed to build. Same vendor groups, working syntax.
        manualChunks(id: string) {
          if (/[\\/]node_modules[\\/](react|react-dom|react-router|react-router-dom)[\\/]/.test(id)) return 'vendor-react'
          if (/[\\/]node_modules[\\/]@xterm[\\/]/.test(id)) return 'vendor-term'
          if (/[\\/]node_modules[\\/]lucide-react[\\/]/.test(id)) return 'vendor-icons'
        },
      },
    },
    // Vite's default modulepreload walks every transitively-reachable
    // async chunk and emits a <link rel="modulepreload"> for each, so
    // xterm would preload on every page just because the Terminal
    // route eventually pulls it in. Excluding it costs one extra RTT
    // when that route is actually visited.
    modulePreload: {
      resolveDependencies: (_filename, deps) =>
        deps.filter((d) => !d.includes('vendor-term')),
    },
  },
  server: {
    allowedHosts: true,
    proxy: {
      // Backend API target. Defaults to the local Rust server on :8788;
      // set DASHUSB_API (e.g. http://dashusb.local) to develop the
      // UI against a live Pi without running the backend locally.
      '/api': process.env.DASHUSB_API || 'http://localhost:8788',
      '/Recordings': process.env.DASHUSB_API || 'http://localhost:8788',
    },
  },
})
