import { describe, expect, it } from "vitest";
import README from "../../../README.md?raw";

describe("README 1.0.0 发布边界", () => {
  it("能力矩阵区分已实现功能和未实现功能，并链接实际发版证据", () => {
    expect(README).toContain("## 当前能力矩阵（1.0.0）");
    expect(README).toContain("release-readiness-1.0.0.md");
    const matrix = README.split("## 当前能力矩阵（1.0.0）")[1].split("## 仓库结构")[0];
    for (const capability of ["AI 对话", "MCP Server / Client", "串口（UART）", "RDP", "系统监控"]) {
      expect(matrix.split("\n").find((line) => line.startsWith("|") && line.includes(capability))).toContain("✅");
    }
    for (const capability of ["手机控制", "SSH Agent forwarding", "SFTP 跟随交互终端 sudo su"]) {
      expect(matrix.split("\n").find((line) => line.startsWith("|") && line.includes(capability))).toContain("❌");
    }
  });

  it("不将旧路线图或未做的真机验收写成当前承诺", () => {
    const intro = README.split("## 仓库结构")[0];
    expect(intro).not.toContain("完全体并超越");
    expect(intro).not.toContain("UI 已禁用");
    expect(intro).not.toContain("本仓库当前是 Phase 1 MVP");
    expect(intro).toContain("仍需真机验收");
  });
});
