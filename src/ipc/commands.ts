import { invoke } from "@tauri-apps/api/core";

export interface AppInfo {
  version: string;
  data_dir: string;
}

export interface MissionInfo {
  id: string;
  title: string;
  description: string;
  status: string;
  total_cost_usd: number;
  created_at: string;
}

export interface CreateMissionRequest {
  title: string;
  description: string;
}

export const commands = {
  getAppInfo: () => invoke<AppInfo>("get_app_info"),

  getDbStatus: () => invoke<string>("get_db_status"),

  createMission: (request: CreateMissionRequest) =>
    invoke<MissionInfo>("create_mission", { request }),

  listMissions: () => invoke<MissionInfo[]>("list_missions"),
};
