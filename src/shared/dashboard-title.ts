import { resolveAnimeTitleDisplay } from "./anime-title";
import type { DashboardData, MyAnime } from "./domain";

interface AnimeTitlePreference {
  sourceTitle: string;
  displayTitle: string;
}

/** 按当前简体中文标题策略统一首页提醒、单集摘要和待处理文案。 */
export function localizeDashboardAnimeTitles(
  dashboard: DashboardData,
  myAnime: readonly MyAnime[]
): DashboardData {
  const preferencesByAnimeId = new Map<string, AnimeTitlePreference>();
  const preferencesBySourceTitle = new Map<string, AnimeTitlePreference>();
  for (const item of myAnime) {
    const preference = {
      sourceTitle: item.anime.title,
      displayTitle: resolveAnimeTitleDisplay(item.anime).title
    };
    preferencesByAnimeId.set(item.anime.id, preference);
    preferencesBySourceTitle.set(preference.sourceTitle, preference);
  }

  const reminderTitles = new Map<string, string>();
  const reminderItems = dashboard.dailyReminder.items.map((item) => {
    const preference = preferencesByAnimeId.get(item.animeId);
    const animeTitle = preference?.displayTitle ?? item.animeTitle;
    reminderTitles.set(item.id, animeTitle);
    return animeTitle === item.animeTitle ? item : { ...item, animeTitle };
  });

  const localizeEpisodeTitle = <T extends { id: string; animeTitle: string }>(item: T): T => {
    const animeTitle = reminderTitles.get(item.id)
      ?? preferencesBySourceTitle.get(item.animeTitle)?.displayTitle
      ?? item.animeTitle;
    return animeTitle === item.animeTitle ? item : { ...item, animeTitle };
  };

  const pendingActions = dashboard.pendingActions.map((item) => {
    const preference = item.animeId ? preferencesByAnimeId.get(item.animeId) : undefined;
    if (!preference || preference.sourceTitle === preference.displayTitle) {
      return item;
    }
    return {
      ...item,
      title: replaceTitle(item.title, preference),
      description: replaceTitle(item.description, preference)
    };
  });

  return {
    ...dashboard,
    dailyReminder: { ...dashboard.dailyReminder, items: reminderItems },
    todayEpisodes: dashboard.todayEpisodes.map(localizeEpisodeTitle),
    pendingActions,
    weeklySchedule: dashboard.weeklySchedule.map((day) => ({
      ...day,
      items: day.items.map(localizeEpisodeTitle)
    }))
  };
}

function replaceTitle(value: string, preference: AnimeTitlePreference): string {
  return value.includes(preference.sourceTitle)
    ? value.replaceAll(preference.sourceTitle, preference.displayTitle)
    : value;
}
