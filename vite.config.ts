import { defineConfig } from "vite";
import vue from "@vitejs/plugin-vue";

// Tauri requires a fixed port; PRD/memory dictates 1420.
// 临时避让 1420(被本机另一 vite 进程占着);改成 1421 + strictPort:false,
// 等对端释放再改回。devUrl 同步指向 1421。
export default defineConfig({
  plugins: [vue()],
  clearScreen: false,
  server: {
    port: 1421,
    strictPort: false,
    host: "0.0.0.0",
    watch: {
      ignored: ["**/src-tauri/**"],
    },
  },
  envPrefix: ["VITE_", "TAURI_"],
  // 预打包重依赖，避免运行中首次进入「图谱」视图时 Vite 临时优化 + 整页 reload，
  // 那会让 Tauri 误判 beforeDevCommand 退出而整个 dev 栈崩掉。
  optimizeDeps: {
    include: ["cytoscape", "cytoscape-fcose", "marked"],
  },
  build: {
    target: "esnext",
    minify: "esbuild",
    sourcemap: false,
    rollupOptions: {
      output: {
        // 只把 cytoscape 显式拆成 graph chunk(它体积大,只服务图谱视图)。
        // 其余依赖交还 Rollup 按动态 import 自动拆分 —— 关键:shiki/katex 是
        // 懒加载的(见 lib/markdown.ts),绝不能用 vendor catch-all 把它们吸进首屏 chunk,
        // 否则 9.7MB 的 shiki 全量语法包会在启动时即被拉取,反而拖慢首屏。
        manualChunks(id) {
          if (id.includes("node_modules/cytoscape")) return "graph";
        },
      },
    },
  },
});
