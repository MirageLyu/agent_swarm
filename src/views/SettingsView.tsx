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

  useEffect(() => {
    commands.getConfig().then(setConfig).catch(console.error);
  }, []);

  const handleSaveKey = async () => {
    if (!apiKey.trim()) return;
    setSaving(true);
    try {
      await commands.setApiKey({ provider: "anthropic", key: apiKey.trim() });
      setConfig((c) => (c ? { ...c, has_anthropic_key: true } : c));
      setApiKey("");
      setMessage("API key saved");
      setTimeout(() => setMessage(""), 2000);
    } catch (e) {
      setMessage(`Error: ${e}`);
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className={styles.container}>
      <div className={styles.section}>
        <h2 className={styles.sectionTitle}>API Keys</h2>
        <div className={styles.field}>
          <div className={styles.fieldHeader}>
            <span>Anthropic</span>
            <Badge variant={config?.has_anthropic_key ? "success" : "warning"}>
              {config?.has_anthropic_key ? "Configured" : "Not Set"}
            </Badge>
          </div>
          <div className={styles.fieldRow}>
            <Input
              type="password"
              placeholder="sk-ant-..."
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
        <h2 className={styles.sectionTitle}>Model</h2>
        <div className={styles.field}>
          <div className={styles.fieldHeader}>
            <span>Default Model</span>
          </div>
          <p className={styles.fieldValue}>{config?.default_model ?? "Loading..."}</p>
        </div>
      </div>

      <div className={styles.section}>
        <h2 className={styles.sectionTitle}>Agents</h2>
        <div className={styles.field}>
          <div className={styles.fieldHeader}>
            <span>Max Concurrent Agents</span>
          </div>
          <p className={styles.fieldValue}>{config?.max_concurrent_agents ?? "..."}</p>
        </div>
      </div>
    </div>
  );
}
