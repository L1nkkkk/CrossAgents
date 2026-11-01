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

/** `mono`: a one-colour mark drawn in `currentColor`, which an image cannot inherit, so it is shown as a mask. */
export type BrandIcon = { src: string; mono?: boolean };

/** By model family (lib/modelFamily.ts). */
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
};

/** By the vendor that runs a plugin. */
export const VENDOR_ICONS: Record<string, BrandIcon> = {
  claude: { src: claude },
  codex: { src: codex },
  codebuddy: { src: codebuddy },
};
