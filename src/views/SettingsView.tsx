import { useEffect, useState } from "react";
import { Button } from "../components/ui/Button";
import { Input } from "../components/ui/Input";
import { Badge } from "../components/ui/Badge";
import { commands, type ConfigResponse } from "../ipc";
import styles from "./SettingsView.module.css";

export function SettingsView() {
  const [config, setConfig] = useState<ConfigResponse | null>(null);
  const [apiKey, setApiKey] = useState("");
  const [saving, setSaving] = useState(false);
  const [message, setMessage] = useState("");

  const [provider, setProvider] = useState("");
  const [baseUrl, setBaseUrl] = useState("");
  const [defaultModel, setDefaultModel] = useState("");
  const [maxAgents, setMaxAgents] = useState("");
  const [configDirty, setConfigDirty] = useState(false);

  useEffect(() => {
    commands.getConfig().then((c) => {
      setConfig(c);
      setProvider(c.provider);
      setBaseUrl(c.base_url);
      setDefaultModel(c.default_model);
      setMaxAgents(String(c.max_concurrent_agents));
    }).catch(console.error);
  }, []);

  const handleSaveKey = async () => {
    if (!apiKey.trim() || !config) return;
    setSaving(true);
    try {
      await commands.setApiKey({ provider: config.provider, key: apiKey.trim() });
      setConfig((c) => (c ? { ...c, has_api_key: true } : c));
      setApiKey("");
      setMessage("API key saved");
      setTimeout(() => setMessage(""), 2000);
    } catch (e) {
      setMessage(`Error: ${e}`);
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
      });
      setConfigDirty(false);
      setMessage("Configuration saved");
      setTimeout(() => setMessage(""), 2000);
    } catch (e) {
      setMessage(`Error: ${e}`);
    } finally {
      setSaving(false);
    }
  };

  const markDirty = () => setConfigDirty(true);

  return (
    <div className={styles.container}>
      <div className={styles.section}>
        <h2 className={styles.sectionTitle}>LLM Provider</h2>
        <div className={styles.field}>
          <div className={styles.fieldHeader}>
            <span>Provider</span>
          </div>
          <Input
            value={provider}
            onChange={(e) => { setProvider(e.target.value); markDirty(); }}
            placeholder="openai"
          />
        </div>
        <div className={styles.field}>
          <div className={styles.fieldHeader}>
            <span>Base URL</span>
          </div>
          <Input
            value={baseUrl}
            onChange={(e) => { setBaseUrl(e.target.value); markDirty(); }}
            placeholder="https://api.openai.com/v1"
          />
        </div>
        <div className={styles.field}>
          <div className={styles.fieldHeader}>
            <span>Model</span>
          </div>
          <Input
            value={defaultModel}
            onChange={(e) => { setDefaultModel(e.target.value); markDirty(); }}
            placeholder="gpt-4o"
          />
        </div>
      </div>

      <div className={styles.section}>
        <h2 className={styles.sectionTitle}>API Key</h2>
        <div className={styles.field}>
          <div className={styles.fieldHeader}>
            <span>{config?.provider ?? "Provider"}</span>
            <Badge variant={config?.has_api_key ? "success" : "warning"}>
              {config?.has_api_key ? "Configured" : "Not Set"}
            </Badge>
          </div>
          <div className={styles.fieldRow}>
            <Input
              type="password"
              placeholder="sk-..."
              value={apiKey}
              onChange={(e) => setApiKey(e.target.value)}
              style={{ flex: 1 }}
            />
            <Button variant="primary" size="sm" onClick={handleSaveKey} disabled={saving}>
              {saving ? "Saving..." : "Save"}
            </Button>
          </div>
        </div>
        {message && <p className={styles.message}>{message}</p>}
      </div>

      <div className={styles.section}>
        <h2 className={styles.sectionTitle}>Agents</h2>
        <div className={styles.field}>
          <div className={styles.fieldHeader}>
            <span>Max Concurrent Agents</span>
          </div>
          <Input
            type="number"
            value={maxAgents}
            onChange={(e) => { setMaxAgents(e.target.value); markDirty(); }}
            placeholder="4"
          />
        </div>
      </div>

      {configDirty && (
        <div className={styles.saveRow}>
          <Button variant="primary" onClick={handleSaveConfig} disabled={saving}>
            {saving ? "Saving..." : "Save Configuration"}
          </Button>
        </div>
      )}
    </div>
  );
}
