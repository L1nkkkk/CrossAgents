#!/usr/bin/env node
/**
 * doubao-mcp: 把本机豆包桌面版的生图能力，以 MCP 工具的形式交给 CrossAgents 的 agent。
 *
 * 它是一层很薄的壳，包住 dsh-doubao-painter（MIT）的 `tools/paint.mjs`：
 *
 *   node tools/paint.mjs "<prompt>" [--ref <image>]... [--out <dir>] --json
 *
 * 所以它只依赖那条命令行的契约，不依赖插件内部实现——插件换了 bridge.mjs 也不会坏。
 *
 * 环境变量：
 *   DOUBAO_PAINTER_HOME  插件仓库的本地路径（必填）
 *   DOUBAO_CDP_PORT      豆包调试端口，默认 9225
 *   DOUBAO_TIMEOUT_MS    单张图上限，默认 240000
 *   DOUBAO_NODE          跑 paint.mjs 用的 node，默认就是运行本文件的这个
 */

import { spawn } from "node:child_process";
import { existsSync } from "node:fs";
import path from "node:path";
import { createInterface } from "node:readline";

const PROTOCOL_VERSION = "2025-06-18";
const PAINTER_HOME = process.env.DOUBAO_PAINTER_HOME || "";
const PAINT_SCRIPT = PAINTER_HOME
  ? path.join(PAINTER_HOME, "tools", "paint.mjs")
  : "";
const CDP_PORT = process.env.DOUBAO_CDP_PORT || "9225";
const TIMEOUT_MS = Number(process.env.DOUBAO_TIMEOUT_MS || 240000);
const NODE = process.env.DOUBAO_NODE || process.execPath;

const TOOLS = [
  {
    name: "doubao_paint",
    description:
      "用本机豆包桌面客户端生成图片，返回下载好的 2K 无水印原图路径。中文海报、带文字的图渲染准确。" +
      "传 prompt 出单张，传 prompts 数组按顺序批量出图，传 ref_images 走图生图（改图）。" +
      "每次调用 20~30 秒一张，串行执行；豆包每日免费额度用完会失败。",
    inputSchema: {
      type: "object",
      properties: {
        prompt: { type: "string", description: "一句话描述要画的图" },
        prompts: {
          type: "array",
          items: { type: "string" },
          description: "批量出图，按顺序逐张跑；与 prompt 二选一",
        },
        ref_images: {
          type: "array",
          items: { type: "string" },
          description: "图生图：参考图的绝对路径，可多张",
        },
        out: {
          type: "string",
          description: "输出目录，默认当前工作区的 doubao-images/",
        },
      },
      anyOf: [{ required: ["prompt"] }, { required: ["prompts"] }],
    },
  },
  {
    name: "doubao_status",
    description:
      "检查豆包生图是否可用：插件脚本在不在、豆包有没有带调试端口运行。出图失败时先调它。",
    inputSchema: { type: "object", properties: {} },
  },
];

/** 跑一次 paint.mjs，返回 { ok } 或 { error }。 */
function paintOnce(args) {
  return new Promise((resolve) => {
    if (!PAINT_SCRIPT || !existsSync(PAINT_SCRIPT)) {
      resolve({
        error:
          "找不到 paint.mjs。设置 DOUBAO_PAINTER_HOME 为 dsh-doubao-painter 的本地路径。",
      });
      return;
    }
    const child = spawn(NODE, [PAINT_SCRIPT, ...args], {
      cwd: process.cwd(),
      env: { ...process.env, PYTHONIOENCODING: "utf-8" },
      windowsHide: true,
    });
    let stdout = "";
    let stderr = "";
    let done = false;
    const finish = (value) => {
      if (done) return;
      done = true;
      clearTimeout(timer);
      resolve(value);
    };
    const timer = setTimeout(() => {
      child.kill();
      finish({ error: `等了 ${Math.round(TIMEOUT_MS / 1000)} 秒还没出图，已放弃` });
    }, TIMEOUT_MS);
    child.stdout.on("data", (chunk) => (stdout += chunk));
    child.stderr.on("data", (chunk) => (stderr += chunk));
    child.on("error", (err) => finish({ error: `起不来 ${NODE}: ${err.message}` }));
    child.on("close", (code) => {
      if (code !== 0) {
        const tail = stderr.trim().split("\n").slice(-8).join("\n");
        finish({
          error: `paint.mjs 退出码 ${code}${tail ? `\n${tail}` : ""}`,
        });
        return;
      }
      finish({ ok: parseOutput(stdout) });
    });
  });
}

/** paint.mjs --json 的结果；容忍前面混了日志行。 */
function parseOutput(stdout) {
  const text = stdout.trim();
  if (!text) return { raw: "" };
  try {
    return JSON.parse(text);
  } catch {
    const lines = text.split("\n").reverse();
    for (const line of lines) {
      const trimmed = line.trim();
      if (!trimmed.startsWith("{")) continue;
      try {
        return JSON.parse(trimmed);
      } catch {
        // 继续往上找
      }
    }
    return { raw: text.slice(-2000) };
  }
}

function filesOf(result) {
  const files = result?.files ?? result?.file ?? result?.outputs ?? [];
  if (Array.isArray(files)) return files.filter((f) => typeof f === "string");
  return typeof files === "string" ? [files] : [];
}

async function callPaint(params) {
  const prompts = Array.isArray(params.prompts)
    ? params.prompts.filter((p) => typeof p === "string" && p.trim())
    : [];
  if (!prompts.length && typeof params.prompt === "string" && params.prompt.trim()) {
    prompts.push(params.prompt);
  }
  if (!prompts.length) {
    return toolError("要给 prompt 或 prompts。");
  }
  const refs = Array.isArray(params.ref_images)
    ? params.ref_images.filter((p) => typeof p === "string")
    : [];
  const args = [];
  for (const ref of refs) args.push("--ref", ref);
  if (typeof params.out === "string" && params.out.trim()) args.push("--out", params.out);

  const files = [];
  let seconds = 0;
  for (const prompt of prompts) {
    const outcome = await paintOnce([prompt, ...args, "--json"]);
    if (outcome.error) {
      // 已经出来的图仍然交付：失败一张不该把整批丢掉。
      return toolResult(
        {
          files,
          seconds: round(seconds),
          error: outcome.error,
          hint: "先调 doubao_status：豆包需要带 --remote-debugging-port 启动，且今日额度未用完。",
        },
        files.length === 0,
      );
    }
    const result = outcome.ok || {};
    files.push(...filesOf(result));
    if (typeof result.seconds === "number") seconds += result.seconds;
    if (result.error) {
      return toolResult({ files, seconds: round(seconds), error: result.error }, files.length === 0);
    }
  }
  return toolResult({ files, seconds: round(seconds) });
}

async function callStatus() {
  const scriptOk = Boolean(PAINT_SCRIPT) && existsSync(PAINT_SCRIPT);
  let cdp = "unknown";
  let cdpDetail = "";
  try {
    const resp = await fetch(`http://127.0.0.1:${CDP_PORT}/json/version`, {
      signal: AbortSignal.timeout(3000),
    });
    if (resp.ok) {
      const info = await resp.json();
      cdp = "ok";
      cdpDetail = info.Browser || "";
    } else {
      cdp = "no";
      cdpDetail = `HTTP ${resp.status}`;
    }
  } catch (err) {
    cdp = "no";
    cdpDetail = err.message;
  }
  const lines = [
    `painter 脚本: ${scriptOk ? PAINT_SCRIPT : "缺失（设置 DOUBAO_PAINTER_HOME）"}`,
    `调试端口 ${CDP_PORT}: ${cdp === "ok" ? `可用 ${cdpDetail}` : `不可用（${cdpDetail}）`}`,
  ];
  if (cdp !== "ok") {
    lines.push(
      `处理: 托盘退出豆包，然后 "…\\Application\\app\\Doubao.exe" --remote-debugging-port=${CDP_PORT}`,
    );
  }
  return { content: [{ type: "text", text: lines.join("\n") }] };
}

function round(value) {
  return Math.round(value * 10) / 10;
}

function toolResult(payload, isError = false) {
  return {
    content: [{ type: "text", text: JSON.stringify(payload, null, 2) }],
    isError,
  };
}

function toolError(message) {
  return { content: [{ type: "text", text: message }], isError: true };
}

async function handle(method, params) {
  switch (method) {
    case "initialize":
      return {
        protocolVersion: params?.protocolVersion || PROTOCOL_VERSION,
        capabilities: { tools: {} },
        serverInfo: { name: "doubao", version: "0.1.0" },
        instructions:
          "本机豆包桌面版的生图工具。doubao_paint 画图并返回本地原图路径；先 doubao_status 排障。",
      };
    case "ping":
      return {};
    case "tools/list":
      return { tools: TOOLS };
    case "tools/call": {
      const name = params?.name;
      const args = params?.arguments || {};
      if (name === "doubao_paint") return await callPaint(args);
      if (name === "doubao_status") return await callStatus();
      throw Object.assign(new Error(`没有这个工具: ${name}`), { code: -32602 });
    }
    default:
      throw Object.assign(new Error(`不支持的方法: ${method}`), { code: -32601 });
  }
}

function send(message) {
  process.stdout.write(`${JSON.stringify(message)}\n`);
}

// 一行一条消息；串行处理，保证响应顺序与请求一致。
let queue = Promise.resolve();
const lines = createInterface({ input: process.stdin, crlfDelay: Infinity });
lines.on("line", (line) => {
  if (!line.trim()) return;
  let message;
  try {
    message = JSON.parse(line);
  } catch {
    return;
  }
  if (message.id === undefined || message.id === null) return; // 通知，不回
  queue = queue.then(async () => {
    try {
      const result = await handle(message.method, message.params);
      send({ jsonrpc: "2.0", id: message.id, result });
    } catch (err) {
      send({
        jsonrpc: "2.0",
        id: message.id,
        error: { code: err.code ?? -32603, message: String(err.message || err) },
      });
    }
  });
});
