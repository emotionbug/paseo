import { withUnistyles } from "react-native-unistyles";
import {
  Archive,
  ArrowDownUp,
  Download,
  GitCommitHorizontal,
  GitMerge,
  RefreshCcw,
  Upload,
} from "lucide-react-native";
import type { Theme } from "@/styles/theme";

const mutedColorMapping = (theme: Theme) => ({ color: theme.colors.foregroundMuted });

const ThemedGitCommitHorizontal = withUnistyles(GitCommitHorizontal, mutedColorMapping);
const ThemedDownload = withUnistyles(Download, mutedColorMapping);
const ThemedUpload = withUnistyles(Upload, mutedColorMapping);
const ThemedArrowDownUp = withUnistyles(ArrowDownUp, mutedColorMapping);
const ThemedGitMerge = withUnistyles(GitMerge, mutedColorMapping);
const ThemedRefreshCcw = withUnistyles(RefreshCcw, mutedColorMapping);
const ThemedArchive = withUnistyles(Archive, mutedColorMapping);

export const GIT_ACTION_ICONS = {
  commit: <ThemedGitCommitHorizontal size={16} />,
  pull: <ThemedDownload size={16} />,
  push: <ThemedUpload size={16} />,
  pullAndPush: <ThemedArrowDownUp size={16} />,
  merge: <ThemedGitMerge size={16} />,
  mergeFromBase: <ThemedRefreshCcw size={16} />,
  archive: <ThemedArchive size={16} />,
};
