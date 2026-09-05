/** S309：快速命令集/片段库的纯函数判据（M4a 出口标准「参数占位符替换单测」）。 */
import { describe, expect, it } from "vitest";
import {
  AI_CATEGORY,
  QC_COMMAND_MAX,
  QC_NAME_MAX,
  parsePlaceholders,
  fillPlaceholders,
  parseQuickCommands,
  groupByCategory,
  quickCommandFromAi,
  type QuickCommand,
} from "./quick-commands";

describe("parsePlaceholders（S309）", () => {
  it("按首现序提取去重的占位符名", () => {
    expect(parsePlaceholders("systemctl restart {{svc}} && journalctl -u {{svc}} -n {{lines}}")).toEqual([
      "svc",
      "lines",
    ]);
  });

  it("不用 ${} / {} 语法——与 shell 变量及花括号展开不撞车", () => {
    expect(parsePlaceholders("echo ${PATH}")).toEqual([]);
    expect(parsePlaceholders("echo {a,b}.txt")).toEqual([]);
    expect(parsePlaceholders("awk '{print $1}'")).toEqual([]);
  });

  it("空名与纯空白名不算占位符（{{}} 是误输入，不是参数）", () => {
    expect(parsePlaceholders("a {{}} b {{   }} c")).toEqual([]);
  });
});

describe("fillPlaceholders（S309）", () => {
  it("值做字面量替换，不解释转义（转义语义归 shell）", () => {
    expect(fillPlaceholders("echo {{msg}}", { msg: `a"b\\c` })).toEqual(`echo a"b\\c`);
  });

  it("未提供值的占位符原样保留——静默吞掉会把 restart {{svc}} 变成重启字面量服务", () => {
    expect(fillPlaceholders("systemctl restart {{svc}} --now", {})).toEqual(
      "systemctl restart {{svc}} --now",
    );
    expect(fillPlaceholders("a {{x}} b {{y}}", { x: "1" })).toEqual("a 1 b {{y}}");
  });

  it("同一占位符多次出现全部替换；名两侧空白容差", () => {
    expect(fillPlaceholders("kill {{pid}}; wait {{ pid }}", { pid: "42" })).toEqual("kill 42; wait 42");
  });

  it("值里的 {{...}} 不再展开（无递归——注入形状收敛为字面量）", () => {
    expect(fillPlaceholders("echo {{x}}", { x: "{{y}}" })).toEqual("echo {{y}}");
  });
});

describe("parseQuickCommands（读侧兜底）", () => {
  it("合法库往返；烂值不抛返回空库（便利功能不为坏一行全炸）", () => {
    const json = JSON.stringify([{ id: "q1", name: "磁盘", command: "df -h", category: "诊断" }]);
    expect(parseQuickCommands(json)).toEqual([{ id: "q1", name: "磁盘", command: "df -h", category: "诊断" }]);
    expect(parseQuickCommands(null)).toEqual([]);
    expect(parseQuickCommands("not json")).toEqual([]);
    expect(parseQuickCommands('{"a":1}')).toEqual([]);
    expect(parseQuickCommands('[{"id":1,"name":"x","command":"ls"}]')).toEqual([]); // 字段类型不对的条目剔除
  });
});

describe("groupByCategory（QuickBar 渲染序）", () => {
  it("分类按首现序、段内按库序、无分类段最后", () => {
    const items = [
      { id: "1", name: "a", command: "ls", category: "诊断" },
      { id: "2", name: "b", command: "ls" },
      { id: "3", name: "c", command: "ls", category: "诊断" },
      { id: "4", name: "d", command: "ls", category: "运维" },
    ];
    expect(groupByCategory(items)).toEqual([
      { category: "诊断", items: [items[0], items[2]] },
      { category: "运维", items: [items[3]] },
      { category: "", items: [items[1]] },
    ]);
  });

  it("空白分类等同无分类", () => {
    expect(groupByCategory([{ id: "1", name: "a", command: "ls", category: "  " }])).toEqual([
      { category: "", items: [{ id: "1", name: "a", command: "ls", category: "  " }] },
    ]);
  });
});

/**
 * AI 生成片段入库（M4b 出口第 2 项后半）。
 *
 * 这一节最要紧的一条是 `denied` 那条：被闸门拒掉的命令不许进库。
 * 其余几条是「不静默修正」这个原则在四种越界情形上的落点。
 */
describe("quickCommandFromAi（M4b：AI 生成片段入库）", () => {
  const none: QuickCommand[] = [];

  it("允许执行的三档都能入库，且落在 AI 分类里", () => {
    for (const d of ["auto", "confirm", "strong-confirm"]) {
      const r = quickCommandFromAi("ss -ltnp", d, none);
      expect(r.ok, d).toBe(true);
      if (r.ok) {
        expect(r.item.command).toBe("ss -ltnp");
        // 固定分类：AI 给的命令与自己写的可信度不同，用户按下去之前
        // 有权知道这一条是谁写的。
        expect(r.item.category).toBe(AI_CATEGORY);
        expect(r.item.id).toBeTruthy();
      }
    }
  });

  it("**被闸门拒掉的命令不许入库**——这是本函数存在的主要理由", () => {
    // 档位是会变的。今天以 read_only 档拒掉的一条 rm -rf，存进去之后
    // 哪天用户切到 with_confirm，它就成了一个点两下就能跑的按钮。
    const r = quickCommandFromAi("rm -rf /var/lib/x", "deny", none);
    expect(r.ok).toBe(false);
    if (!r.ok) expect(r.reason).toBe("denied");
  });

  it("未知的裁决档位也拒——白名单而不是黑名单", () => {
    // 将来多一个档位时，黑名单会默认放行它。而一个我们不认识的裁决，
    // 默认必须是「不入库」。
    for (const d of ["", "maybe", "AUTO", "auto ", "allow", "unknown-tier"]) {
      const r = quickCommandFromAi("ls", d, none);
      expect(r.ok, `裁决 ${JSON.stringify(d)} 不该放行`).toBe(false);
    }
  });

  it("空命令与超长命令都拒，不静默修正", () => {
    expect(quickCommandFromAi("", "auto", none).ok).toBe(false);
    expect(quickCommandFromAi("   \n  ", "auto", none).ok).toBe(false);
    const tooLong = "echo " + "x".repeat(QC_COMMAND_MAX);
    const r = quickCommandFromAi(tooLong, "auto", none);
    expect(r.ok).toBe(false);
    if (!r.ok) expect(r.reason).toBe("too-long");
    // 边界：正好等于上限要放行（越界判定用 > 而不是 >=）
    expect(quickCommandFromAi("x".repeat(QC_COMMAND_MAX), "auto", none).ok).toBe(true);
  });

  it("截断一条 shell 命令得到的是另一条命令——所以不截断", () => {
    // `rm -rf /tmp/x` 截成 `rm -rf /tmp` 仍然合法，语义完全不同。
    // 这条钉的是「拒绝」而不是「修正」这个选择本身。
    const long = "rm -rf /tmp/" + "a".repeat(QC_COMMAND_MAX);
    const r = quickCommandFromAi(long, "confirm", none);
    expect(r.ok).toBe(false);
  });

  it("命令体逐字相同的重复不入库", () => {
    const existing: QuickCommand[] = [
      { id: "1", name: "看端口", command: "ss -ltnp", category: "网络" },
    ];
    const r = quickCommandFromAi("ss -ltnp", "auto", existing);
    expect(r.ok).toBe(false);
    if (!r.ok) expect(r.reason).toBe("duplicate");
    // 前后空白不同视为同一条（trim 之后比）
    expect(quickCommandFromAi("  ss -ltnp  ", "auto", existing).ok).toBe(false);
    // 但内容不同就是新的一条，哪怕只差一个参数
    expect(quickCommandFromAi("ss -ltnpa", "auto", existing).ok).toBe(true);
  });

  it("名字压成单行并截到上限——片段名显示在一个按钮上", () => {
    const multi = "ps aux |\n  grep nginx |\n  awk '{print $2}'";
    const r = quickCommandFromAi(multi, "auto", none);
    expect(r.ok).toBe(true);
    if (r.ok) {
      expect(r.item.name).not.toContain("\n");
      expect(r.item.name.length).toBeLessThanOrEqual(QC_NAME_MAX);
      // 命令体**保留**原样，包括换行——被截的只是显示名
      expect(r.item.command).toBe(multi);
    }
    const long = "echo " + "y".repeat(200);
    const r2 = quickCommandFromAi(long, "auto", none);
    expect(r2.ok).toBe(true);
    if (r2.ok) {
      expect(r2.item.name).toHaveLength(QC_NAME_MAX);
      expect(r2.item.command).toBe(long);
    }
  });

  it("生成的片段能过 parseQuickCommands（入库后读得回来）", () => {
    // 契约闭环：写进去的形状必须是读侧认的形状。两侧各写一次而不校验，
    // 结果是「存成功了但列表里看不见」——而那时用户会再存一次。
    const r = quickCommandFromAi("df -h", "auto", none);
    expect(r.ok).toBe(true);
    if (!r.ok) return;
    const back = parseQuickCommands(JSON.stringify([r.item]));
    expect(back).toHaveLength(1);
    expect(back[0]).toEqual(r.item);
  });
});
