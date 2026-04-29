import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import "./styles/global.css";
// i18n 必须在任何使用 useTranslation 的组件被渲染前 import 一次以触发 init。
// 实际的 changeLanguage 在 App 启动时根据 backend config 同步。
import "./i18n";

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
