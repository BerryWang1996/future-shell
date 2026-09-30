import { defineConfig } from "vitest/config";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import { svelteTesting } from "@testing-library/svelte/vite";

export default defineConfig({
  plugins: [svelte(), svelteTesting()],
  // settings-wiring.test.ts 要读 Rust 侧源码（app/src、crates）来闭合「设置键的写入方 ↔ 消费方」，
  // 而消费方有一半在后端。Vite 默认只许加载 root（= frontend/）以下的文件，故此处放开到仓库根。
  // **只在 vitest 配置里放开**：vite.config.ts（dev server / 生产构建）保持默认收紧的 fs.allow，
  // 这条不影响任何对外暴露的路径。
  server: { fs: { allow: [".."] } },
  test: {
    environment: "jsdom",
    include: ["src/**/*.test.ts"],
    setupFiles: ["./src/test-setup.ts"], // S266：jsdom 的 TextEncoder 与 Uint8Array 跨 realm，见该文件头注
    globals: true, // @testing-library/svelte 自动 cleanup 依赖全局 afterEach；vitest 默认 globals:false 时 afterEach 未定义 → cleanup 不注册 → 组件测试跨例 DOM 累积（Task 18 SearchOverlay 两例、Task 20 TabBar 两例的 findByTestId/getByRole 必撞多命中而红，Task 18 Step 6「累计 90 PASS」门禁不可达）；S268：原文作 81，系计划撰写时 Task 17 尚为 60 例，其后两轮复审补 S262–S265 共 9 例 ⇒ 基线 69 + Task 18 增量 21 + Task 21 Step 1 增量 3（app/sftp_cmd 跨平台逃逸覆盖，unix symlink 第 4 件不计）= 93（2026-08-10 local gate 取数）
  },
});
