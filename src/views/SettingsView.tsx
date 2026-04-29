import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "../components/ui/Button";
import { Input } from "../components/ui/Input";
import { Badge } from "../components/ui/Badge";
import { ApprovalPolicySection } from "../components/approval";
import { DiagnosticsSection } from "../components/settings/DiagnosticsSection";
import { LanguageSection } from "../components/settings/LanguageSection";
import { commands, type ConfigResponse } from "../ipc";
import { formatBackendError } from "../i18n";
import { useUiStore } from "../stores/ui-store";
import styles from "./SettingsView.module.css";

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
  const [configDirty, setConfigDirty] = useState(false);

  useEffect(() => {
    commands
      .getConfig()
      .then((c) => {
        setConfig(c);
        setProvider(c.provider);
        setBaseUrl(c.base_url);
        setDefaultModel(c.default_model);
        setMaxAgents(String(c.max_concurrent_agents));
        setMaxSteps(String(c.max_agent_steps));
        setAgentTimeout(String(c.agent_timeout_seconds));
        setStepIdle(String(c.agent_step_idle_seconds));
      })
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
      });
      setConfigDirty(false);
      setMessage(t("configSavedHint"));
      setTimeout(() => setMessage(""), 3000);
    } catch (e) {
      setMessage(tc("errorPrefix", { message: formatBackendError(e) }));
    } finally {
      setSaving(false);
    }
  };

  const markDirty = () => setConfigDirty(true);

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
            onChange={(e) => {
              setProvider(e.target.value);
              markDirty();
            }}
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
            onChange={(e) => {
              setBaseUrl(e.target.value);
              markDirty();
            }}
            placeholder={t("baseUrlPlaceholder")}
          />
        </div>
        <div className={styles.field}>
          <div className={styles.fieldHeader}>
            <span>{t("modelLabel")}</span>
          </div>
          <Input
            value={defaultModel}
            onChange={(e) => {
              setDefaultModel(e.target.value);
              markDirty();
            }}
            placeholder={t("modelPlaceholder")}
          />
        </div>
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
            onChange={(e) => {
              setMaxAgents(e.target.value);
              markDirty();
            }}
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
            onChange={(e) => {
              setMaxSteps(e.target.value);
              markDirty();
            }}
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
            onChange={(e) => {
              setAgentTimeout(e.target.value);
              markDirty();
            }}
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
            onChange={(e) => {
              setStepIdle(e.target.value);
              markDirty();
            }}
            placeholder="60"
          />
          <p className={styles.hint}>{t("stepIdleHint")}</p>
        </div>
      </div>

      {configDirty && (
        <div className={styles.saveRow}>
          <Button variant="primary" onClick={handleSaveConfig} disabled={saving}>
            {saving ? tc("saving") : t("saveConfig")}
          </Button>
        </div>
      )}

      <LanguageSection />
      <ApprovalPolicySection />
      <DiagnosticsSection />
    </div>
  );
}
