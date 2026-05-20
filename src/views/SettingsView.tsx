import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "../components/ui/Button";
import { Input } from "../components/ui/Input";
import { Badge } from "../components/ui/Badge";
import { ApprovalPolicySection } from "../components/approval";
import { DeveloperSection } from "../components/settings/DeveloperSection";
import { DiagnosticsSection } from "../components/settings/DiagnosticsSection";
import { LanguageSection } from "../components/settings/LanguageSection";
import {
  commands,
  type ConfigResponse,
  type TestLlmConnectionResponse,
} from "../ipc";
import { formatBackendError } from "../i18n";
import { useUiStore } from "../stores/ui-store";
import styles from "./SettingsView.module.css";

type TestResult =
  | { state: "idle" }
  | { state: "running" }
  | { state: "ok"; data: TestLlmConnectionResponse }
  | { state: "error"; message: string };

export function SettingsView() {
  const { t } = useTranslation("settings");
  const { t: tc } = useTranslation("common");

  const [config, setConfig] = useState<ConfigResponse | null>(null);
  const [apiKey, setApiKey] = useState("");
  const [saving, setSaving] = useState(false);
  const [message, setMessage] = useState("");

  const [provider, setProvider] = useState("");
  const [baseUrl, setBaseUrl] = useState("");
  const [defaultModel, setDefaultModel] = useState("");
  const [maxAgents, setMaxAgents] = useState("");
  const [maxSteps, setMaxSteps] = useState("");
  const [agentTimeout, setAgentTimeout] = useState("");
  const [stepIdle, setStepIdle] = useState("");
  // P0-2 / P1-2: 把这三个字段也纳入 dirty diff，避免改了不保存的体验割裂。
  const [outputTokenBudget, setOutputTokenBudget] = useState("");
  const [fallbackModel, setFallbackModel] = useState("");
  const [fallbackSticky, setFallbackSticky] = useState(true);

  // dirty 通过比较"当前 form 值"与"后端最近一次返回值"派生出来，
  // 避免某次 markDirty 后忘记 reset 导致 Save 按钮卡住。
  const configDirty = !!config && (
    provider !== config.provider ||
    baseUrl !== config.base_url ||
    defaultModel !== config.default_model ||
    maxAgents !== String(config.max_concurrent_agents) ||
    maxSteps !== String(config.max_agent_steps) ||
    agentTimeout !== String(config.agent_timeout_seconds) ||
    stepIdle !== String(config.agent_step_idle_seconds) ||
    outputTokenBudget !== String(config.agent_output_token_budget) ||
    fallbackModel !== config.agent_fallback_model ||
    fallbackSticky !== config.agent_fallback_sticky
  );

  const applyConfig = (c: ConfigResponse) => {
    setConfig(c);
    setProvider(c.provider);
    setBaseUrl(c.base_url);
    setDefaultModel(c.default_model);
    setMaxAgents(String(c.max_concurrent_agents));
    setMaxSteps(String(c.max_agent_steps));
    setAgentTimeout(String(c.agent_timeout_seconds));
    setStepIdle(String(c.agent_step_idle_seconds));
    setOutputTokenBudget(String(c.agent_output_token_budget));
    setFallbackModel(c.agent_fallback_model);
    setFallbackSticky(c.agent_fallback_sticky);
  };

  useEffect(() => {
    commands
      .getConfig()
      .then(applyConfig)
      // eslint-disable-next-line no-console
      .catch(console.error);
  }, []);

  const handleSaveKey = async () => {
    if (!apiKey.trim() || !config) return;
    setSaving(true);
    try {
      await commands.setApiKey({ provider: config.provider, key: apiKey.trim() });
      setConfig((c) => (c ? { ...c, has_api_key: true } : c));
      // 让 MissionsView 顶部的引导 banner 立刻消失
      useUiStore.getState().setApiKeyConfigured(true);
      setApiKey("");
      setMessage(t("saveKeySuccess"));
      setTimeout(() => setMessage(""), 2000);
    } catch (e) {
      setMessage(tc("errorPrefix", { message: formatBackendError(e) }));
    } finally {
      setSaving(false);
    }
  };

  const handleSaveConfig = async () => {
    setSaving(true);
    try {
      await commands.updateConfig({
        provider,
        base_url: baseUrl,
        default_model: defaultModel,
        max_concurrent_agents: parseInt(maxAgents, 10) || 4,
        max_agent_steps: parseInt(maxSteps, 10) || 80,
        agent_timeout_seconds: parseInt(agentTimeout, 10) || 1800,
        agent_step_idle_seconds: Math.max(0, parseInt(stepIdle, 10) || 0),
        // P0-2 / P1-2: empty string + parseInt = NaN，用 || 0 / "" 兜底。
        // 后端 update_config 会 clamp 上限并把空字符串当作 "关闭"。
        agent_output_token_budget: Math.max(0, parseInt(outputTokenBudget, 10) || 0),
        agent_fallback_model: fallbackModel.trim(),
        agent_fallback_sticky: fallbackSticky,
      });
      // 关键：保存后立刻 refetch，让 form 显示后端真实持久化的值
      // （后端会做 clamp / 大小写规范化等修正），这样 dirty 状态自然 reset
      const fresh = await commands.getConfig();
      applyConfig(fresh);
      setMessage(t("configSavedHint"));
      setTimeout(() => setMessage(""), 3000);
    } catch (e) {
      setMessage(tc("errorPrefix", { message: formatBackendError(e) }));
    } finally {
      setSaving(false);
    }
  };

  const handleDiscardChanges = () => {
    if (config) applyConfig(config);
  };

  const [testResult, setTestResult] = useState<TestResult>({ state: "idle" });

  const handleTestConnection = async () => {
    setTestResult({ state: "running" });
    try {
      // 发送当前 form 值（snake_case），后端缺省回退到 saved config，
      // 这样用户不必先保存就能预试新的 provider/url/model 组合。
      const data = await commands.testLlmConnection({
        provider: provider.trim() || undefined,
        base_url: baseUrl.trim() || undefined,
        model: defaultModel.trim() || undefined,
      });
      setTestResult({ state: "ok", data });
    } catch (e) {
      setTestResult({ state: "error", message: formatBackendError(e) });
    }
  };

  return (
    <div className={styles.container}>
      <div className={styles.section}>
        <h2 className={styles.sectionTitle}>{t("providerHeader")}</h2>
        <div className={styles.field}>
          <div className={styles.fieldHeader}>
            <span>{t("providerNameLabel")}</span>
          </div>
          <Input
            value={provider}
            onChange={(e) => setProvider(e.target.value)}
            placeholder={t("providerPlaceholder")}
          />
          <p className={styles.hint}>{t("providerHint")}</p>
        </div>
        <div className={styles.field}>
          <div className={styles.fieldHeader}>
            <span>{t("baseUrlLabel")}</span>
          </div>
          <Input
            value={baseUrl}
            onChange={(e) => setBaseUrl(e.target.value)}
            placeholder={t("baseUrlPlaceholder")}
          />
        </div>
        <div className={styles.field}>
          <div className={styles.fieldHeader}>
            <span>{t("modelLabel")}</span>
          </div>
          <Input
            value={defaultModel}
            onChange={(e) => setDefaultModel(e.target.value)}
            placeholder={t("modelPlaceholder")}
          />
        </div>

        <div className={styles.testRow}>
          <Button
            variant="secondary"
            size="sm"
            onClick={handleTestConnection}
            disabled={testResult.state === "running"}
          >
            {testResult.state === "running" ? t("testingConnection") : t("testConnection")}
          </Button>
          <p className={styles.hint}>{t("testConnectionHint")}</p>
        </div>

        {testResult.state === "ok" && (
          <div className={`${styles.testResult} ${styles.testResultOk}`} role="status">
            <p className={styles.testResultLine}>
              {t("testConnectionSuccess", {
                latency: testResult.data.latency_ms,
                input: testResult.data.usage.input_tokens,
                output: testResult.data.usage.output_tokens,
              })}
            </p>
            {testResult.data.sample_text && (
              <p className={styles.testResultReply}>
                {t("testConnectionReply", { text: testResult.data.sample_text })}
              </p>
            )}
          </div>
        )}
        {testResult.state === "error" && (
          <div className={`${styles.testResult} ${styles.testResultErr}`} role="alert">
            <p className={styles.testResultLine}>{testResult.message}</p>
            <p className={styles.testResultReply}>{t("testConnectionUnsavedHint")}</p>
          </div>
        )}
      </div>

      <div className={styles.section}>
        <h2 className={styles.sectionTitle}>{t("apiKeysHeader")}</h2>
        <p className={styles.hint}>{t("apiKeysIntro")}</p>
        <div className={styles.field}>
          <div className={styles.fieldHeader}>
            <span>{config?.provider ?? t("providerNameLabel")}</span>
            <Badge variant={config?.has_api_key ? "success" : "warning"}>
              {config?.has_api_key ? t("apiKeyConfigured") : t("apiKeyMissing")}
            </Badge>
          </div>
          <div className={styles.fieldRow}>
            <Input
              type="password"
              placeholder={t("apiKeyPlaceholder")}
              value={apiKey}
              onChange={(e) => setApiKey(e.target.value)}
              style={{ flex: 1 }}
            />
            <Button variant="primary" size="sm" onClick={handleSaveKey} disabled={saving}>
              {saving ? tc("saving") : t("saveKey")}
            </Button>
          </div>
        </div>
        {message && <p className={styles.message}>{message}</p>}
      </div>

      <div className={styles.section}>
        <h2 className={styles.sectionTitle}>{t("agentsHeader")}</h2>
        <div className={styles.field}>
          <div className={styles.fieldHeader}>
            <span>{t("maxAgentsLabel")}</span>
          </div>
          <Input
            type="number"
            value={maxAgents}
            onChange={(e) => setMaxAgents(e.target.value)}
            placeholder="4"
          />
        </div>
        <div className={styles.field}>
          <div className={styles.fieldHeader}>
            <span>{t("maxStepsLabel")}</span>
          </div>
          <Input
            type="number"
            value={maxSteps}
            onChange={(e) => setMaxSteps(e.target.value)}
            placeholder="80"
          />
          <p className={styles.hint}>{t("maxStepsHint")}</p>
        </div>
        <div className={styles.field}>
          <div className={styles.fieldHeader}>
            <span>{t("agentTimeoutLabel")}</span>
          </div>
          <Input
            type="number"
            value={agentTimeout}
            onChange={(e) => setAgentTimeout(e.target.value)}
            placeholder="1800"
          />
          <p className={styles.hint}>{t("agentTimeoutHint")}</p>
        </div>
        <div className={styles.field}>
          <div className={styles.fieldHeader}>
            <span>{t("stepIdleLabel")}</span>
          </div>
          <Input
            type="number"
            value={stepIdle}
            onChange={(e) => setStepIdle(e.target.value)}
            placeholder="60"
          />
          <p className={styles.hint}>{t("stepIdleHint")}</p>
        </div>
      </div>

      {/* Single-Agent Uplift P0-2 + P1-2: 这两个 section 紧跟 agents block，
          因为它们调节的是 agent 循环行为而非 provider 凭据。Apple 风格遵循
          "相关配置就近放置"——把它们塞到 LanguageSection 之后会割裂语义。 */}
      <div className={styles.section}>
        <h2 className={styles.sectionTitle}>{t("outputTokenBudgetHeader")}</h2>
        <p className={styles.hint}>{t("outputTokenBudgetIntro")}</p>
        <div className={styles.field}>
          <div className={styles.fieldHeader}>
            <span>{t("outputTokenBudgetLabel")}</span>
          </div>
          <Input
            type="number"
            value={outputTokenBudget}
            onChange={(e) => setOutputTokenBudget(e.target.value)}
            placeholder="0"
            min={0}
            max={1000000}
          />
          <p className={styles.hint}>{t("outputTokenBudgetHint")}</p>
        </div>
      </div>

      <div className={styles.section}>
        <h2 className={styles.sectionTitle}>{t("fallbackHeader")}</h2>
        <p className={styles.hint}>{t("fallbackIntro")}</p>
        <div className={styles.field}>
          <div className={styles.fieldHeader}>
            <span>{t("fallbackModelLabel")}</span>
          </div>
          <Input
            value={fallbackModel}
            onChange={(e) => setFallbackModel(e.target.value)}
            placeholder="(disabled)"
          />
          <p className={styles.hint}>{t("fallbackModelHint")}</p>
        </div>
        <div className={styles.field}>
          <div className={styles.fieldHeader}>
            <span>{t("fallbackStickyLabel")}</span>
          </div>
          <div style={{ display: "flex", gap: "var(--space-2)", marginBottom: "var(--space-2)" }}>
            <Button
              variant={fallbackSticky ? "primary" : "ghost"}
              size="sm"
              onClick={() => setFallbackSticky(true)}
            >
              {t("fallbackStickyOn")}
            </Button>
            <Button
              variant={!fallbackSticky ? "primary" : "ghost"}
              size="sm"
              onClick={() => setFallbackSticky(false)}
            >
              {t("fallbackStickyOff")}
            </Button>
          </div>
          <p className={styles.hint}>{t("fallbackStickyHint")}</p>
        </div>
      </div>

      <LanguageSection />
      <ApprovalPolicySection />
      <DiagnosticsSection />
      <DeveloperSection />

      {/* sticky 底部保存栏：dirty 时浮现，scroll 不掉。
          关键 UX 修复：之前的 saveRow 嵌在 form 中段，
          用户在 Provider 区编辑后看不到隐藏在下面的按钮，误以为"无法保存" */}
      {configDirty && (
        <div className={styles.stickySaveBar} role="region" aria-live="polite">
          <span className={styles.stickyHint}>{t("configDirtyHint")}</span>
          <div className={styles.stickyActions}>
            <Button variant="ghost" size="sm" onClick={handleDiscardChanges} disabled={saving}>
              {t("discardChanges")}
            </Button>
            <Button variant="primary" size="sm" onClick={handleSaveConfig} disabled={saving}>
              {saving ? tc("saving") : t("saveConfig")}
            </Button>
          </div>
        </div>
      )}
    </div>
  );
}
