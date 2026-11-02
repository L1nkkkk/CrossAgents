// The official marks, from @lobehub/icons-static-svg. Only the ones named here reach the bundle.
import claude from "@lobehub/icons-static-svg/icons/claude-color.svg";
import codebuddy from "@lobehub/icons-static-svg/icons/codebuddy-color.svg";
import codex from "@lobehub/icons-static-svg/icons/codex-color.svg";
import deepseek from "@lobehub/icons-static-svg/icons/deepseek-color.svg";
import gemini from "@lobehub/icons-static-svg/icons/gemini-color.svg";
import hunyuan from "@lobehub/icons-static-svg/icons/hunyuan-color.svg";
import kimi from "@lobehub/icons-static-svg/icons/kimi.svg";
import minimax from "@lobehub/icons-static-svg/icons/minimax-color.svg";
import openai from "@lobehub/icons-static-svg/icons/openai.svg";
import qwen from "@lobehub/icons-static-svg/icons/qwen-color.svg";
import step from "@lobehub/icons-static-svg/icons/stepfun-color.svg";
import glm from "@lobehub/icons-static-svg/icons/zhipu-color.svg";
import mimo from "@lobehub/icons-static-svg/icons/xiaomimimo.svg";
import grok from "@lobehub/icons-static-svg/icons/grok.svg";
import doubao from "@lobehub/icons-static-svg/icons/doubao-color.svg";
import wenxin from "@lobehub/icons-static-svg/icons/wenxin-color.svg";
import mistral from "@lobehub/icons-static-svg/icons/mistral-color.svg";
import yi from "@lobehub/icons-static-svg/icons/yi-color.svg";
import baichuan from "@lobehub/icons-static-svg/icons/baichuan-color.svg";
import internlm from "@lobehub/icons-static-svg/icons/internlm-color.svg";
import spark from "@lobehub/icons-static-svg/icons/spark-color.svg";
import sensenova from "@lobehub/icons-static-svg/icons/sensenova-color.svg";
import skywork from "@lobehub/icons-static-svg/icons/skywork-color.svg";
import longcat from "@lobehub/icons-static-svg/icons/longcat-color.svg";

/** `mono`: a one-colour mark drawn in `currentColor`, which an image cannot inherit, so it is shown as a mask. */
export type BrandIcon = { src: string; mono?: boolean };

/** By model family; the keys are lib/modelFamily.ts's family keys. */
export const MODEL_ICONS: Record<string, BrandIcon> = {
  claude: { src: claude },
  openai: { src: openai, mono: true },
  deepseek: { src: deepseek },
  glm: { src: glm },
  step: { src: step },
  gemini: { src: gemini },
  qwen: { src: qwen },
  kimi: { src: kimi, mono: true },
  minimax: { src: minimax },
  hunyuan: { src: hunyuan },
  mimo: { src: mimo, mono: true },
  grok: { src: grok, mono: true },
  doubao: { src: doubao },
  wenxin: { src: wenxin },
  mistral: { src: mistral },
  yi: { src: yi, mono: true },
  baichuan: { src: baichuan },
  internlm: { src: internlm },
  spark: { src: spark },
  sensenova: { src: sensenova },
  skywork: { src: skywork },
  longcat: { src: longcat, mono: true },
};

/** By the vendor that runs a plugin. */
export const VENDOR_ICONS: Record<string, BrandIcon> = {
  claude: { src: claude },
  codex: { src: codex },
  codebuddy: { src: codebuddy },
};
