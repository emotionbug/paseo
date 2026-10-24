import AsyncStorage from "@react-native-async-storage/async-storage";
import { create } from "zustand";
import { persist } from "zustand/middleware";
import { z } from "zod";
import { createValidatedPersistStorage } from "@/storage/validated-persist-storage";

const schema = z.object({ sessionFirst: z.boolean() });
interface SessionNavigationState extends z.infer<typeof schema> {
  toggle: () => void;
}

export const useSessionNavigationStore = create<SessionNavigationState>()(
  persist(
    (set) => ({
      sessionFirst: true,
      toggle: () => set((state) => ({ sessionFirst: !state.sessionFirst })),
    }),
    {
      name: "paseo-session-navigation",
      storage: createValidatedPersistStorage(AsyncStorage, schema),
      partialize: ({ sessionFirst }) => ({ sessionFirst }),
    },
  ),
);
