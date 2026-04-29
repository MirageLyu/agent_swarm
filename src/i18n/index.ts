/**
 * Miragenty i18n 子系统。
 *
 * # 设计要点
 *
 * 1. **单一真源**：language 持久化在 backend `config.json` 的 `language` 字段。
 *    前端启动时 fetch config → init i18next；切换时同时写 backend + i18next。
 *    localStorage 不存语言（避免 backend 与前端不一致的"分裂大脑"问题）。
 *
 * 2. **Namespace 划分**：按"用户接触面"切，不按代码模块切。
 *    - common: 通用按钮/状态/单位/确认弹窗
 *    - nav: 导航栏 / 标题栏
 *    - settings: Settings view
 *    - mission: Missions / mission 子组件 / Plan 流程
 *    - workspace: Workspace view + Agent 面板
 *    - report: Mission Report (FM-12)
 *    - approval: Approval queue (FM-14)
 *    - insights: Insights view (FM-13 lite)
 *    - preflight: Preflight 多轮对话 + 合同编辑
 *    - errors: 后端 error code → 前端文本 mapping
 *
 *    避免 nesting 超过 3 层；超过 3 层一般是 namespace 切错了。
 *
 * 3. **Missing key 行为**：
 *    - 开发期：浏览器控制台输出 warn，并在 UI 上显示 `⟦key⟧` 让漏掉的明显
 *    - 生产期：fallback 到 en-US；en-US 也缺则原样显示 key
 *
 * 4. **Hot swap**：i18next.changeLanguage 触发 react-i18next 自动 re-render；
 *    无需在组件里手动 subscribe。
 *
 * # 添加新字符串的步骤
 *
 * 1. 在 `locales/en-US.json` 对应 namespace 加 key（**先写英文**，因为 fallback）
 * 2. 在 `locales/zh-CN.json` 同 namespace 加翻译
 * 3. 组件里：`const { t } = useTranslation('namespace'); ... t('key')`
 *    或 `t('namespace:key')` 跨 namespace
 * 4. 带变量：JSON 里写 `"hello": "Hello, {{name}}!"`，调用 `t('hello', { name: 'X' })`
 * 5. 复数：`t('items', { count: 5 })`，JSON 用 `items_one` / `items_other` 复数后缀
 *
 * # 加新语言的步骤
 *
 * 1. backend `commands/config.rs::SUPPORTED` 数组追加 BCP 47 tag
 * 2. `src/i18n/locales/<tag>.json` 复制 en-US.json 全文翻译
 * 3. `src/i18n/index.ts::SUPPORTED_LANGUAGES` 追加显示名
 * 4. `resources` 里注册 new locale
 */
import i18n from "i18next";
import { initReactI18next } from "react-i18next";
import enUS from "./locales/en-US.json";
import zhCN from "./locales/zh-CN.json";

export const SUPPORTED_LANGUAGES = [
  { code: "en-US", label: "English" },
  { code: "zh-CN", label: "简体中文" },
] as const;

export type SupportedLanguage = (typeof SUPPORTED_LANGUAGES)[number]["code"];

export const DEFAULT_LANGUAGE: SupportedLanguage = "en-US";

/**
 * Namespace 列表。新增 namespace 时同时更新两个 locale json 和这里。
 * 类型化保证 useTranslation('xxx') 和 t('xxx:key') 都能拿到 IDE 补全。
 */
export const NAMESPACES = [
  "common",
  "nav",
  "settings",
  "mission",
  "workspace",
  "report",
  "approval",
  "insights",
  "preflight",
  "errors",
] as const;

export type Namespace = (typeof NAMESPACES)[number];

const isDev = import.meta.env?.DEV ?? false;

void i18n.use(initReactI18next).init({
  resources: {
    "en-US": enUS,
    "zh-CN": zhCN,
  },
  lng: DEFAULT_LANGUAGE,
  fallbackLng: DEFAULT_LANGUAGE,
  ns: NAMESPACES as unknown as string[],
  defaultNS: "common",
  interpolation: {
    // React 默认会 escape，关掉 i18next 自带的避免双重 escape
    escapeValue: false,
  },
  // 开发期把 missing key 显式标出来
  saveMissing: isDev,
  missingKeyHandler: isDev
    ? (langs, ns, key) => {
        // 仅一次性 warn，避免控制台爆炸
        const tag = `[i18n missing] ns=${ns} key=${key} langs=${langs.join(",")}`;
        // eslint-disable-next-line no-console
        console.warn(tag);
      }
    : undefined,
  parseMissingKeyHandler: isDev
    ? (key, defaultValue) => {
        // 开发期把缺失 key 用 ⟦⟧ 标出来；生产期回退到默认/key
        return defaultValue ?? `⟦${key}⟧`;
      }
    : undefined,
  // 关掉 react suspense（小 app 不需要，避免 boundary 复杂化）
  react: {
    useSuspense: false,
  },
});

export default i18n;

/**
 * 在 React 树外（例如 store / utility）需要翻译时用。
 * 在 React 组件里**必须**用 `useTranslation`，因为它能在 changeLanguage 时触发 re-render。
 */
export function tImperative(
  key: string,
  options?: Record<string, unknown>,
): string {
  return i18n.t(key, options) as string;
}

/**
 * 切换语言的统一入口。
 * - 同步 i18next（触发组件 re-render）
 * - 同步 backend config.json（持久化，下次启动还原）
 *
 * 调用方应处理 backend 错误（如：网络断、IPC 异常）。
 * 即便 backend 写失败，前端 i18next 也已经切了，下一次启动会读旧值还原。
 */
export async function changeLanguage(
  lng: SupportedLanguage,
  persist?: (lng: SupportedLanguage) => Promise<void>,
): Promise<void> {
  await i18n.changeLanguage(lng);
  if (persist) {
    try {
      await persist(lng);
    } catch (e) {
      // 不重抛：UI 上语言已经切了；persist 失败的代价是下次启动还原成旧值
      // eslint-disable-next-line no-console
      console.warn("[i18n] persist failed:", e);
    }
  }
}
