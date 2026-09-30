import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";

// mode 而非 command 做判据：`vite build` 默认就是 production mode，而 `vite build --mode development`
// 是我们排查产物问题时唯一想要 sourcemap 的入口——用 command === "build" 判会把这条路也一起堵死。
export default defineConfig(({ mode }) => ({
  plugins: [svelte()],
  clearScreen: false,
  server: { port: 5173, strictPort: true },
  envPrefix: ["VITE_", "TAURI_"],
  build: {
    target: "es2022",
    outDir: "dist",
    // sourcemap 取舍：开着时 dist 里多出 ~1.96MB 的 .map，而 map 内联了全部前端源码。
    // 本仓是要公开分发安装包的桌面应用（且零遥测——没有崩溃上报服务去消费这些 map），
    // map 跟着装机等于白送一份可读源码给每个用户，对用户零价值、对包体是纯负担。
    // 所以生产构建关掉；确实要对着线上产物看栈时，用 `npx vite build --mode development`
    // 单独出一份带 map 的构建留在本地比对，不进安装包。
    sourcemap: mode !== "production",
    rollupOptions: {
      output: {
        // 主 JS 曾达 ~640KB 并触发 rollup 的大 chunk 警告，绝大部分体积是 xterm 内核 + 6 个 addon。
        // 把它们整体切成一个独立 chunk 的收益：应用自身代码（改动频繁）与终端引擎（几乎不动）
        // 解耦，增量更新时用户只需重下前者；同时主 chunk 回到可审视的量级。
        //
        // 刻意把内核与 addon 合并成同一个 chunk，而不是每个 addon 一个：addon 与内核之间有
        // 模块级的相互引用，拆散后 rollup 会生成跨 chunk 的初始化顺序依赖，历史上是 WebGL
        // addon 这类「构造时就要拿到内核符号」的包最容易踩的坑。合成一块没有这个风险，
        // 而收益（缓存粒度）本来就是按「终端引擎整体」计的。
        manualChunks(id: string) {
          if (id.includes("node_modules/@xterm/")) return "xterm";
          return undefined;
        },
      },
    },
  },
}));
