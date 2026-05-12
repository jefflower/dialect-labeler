/** Auth context — keeps the current user in React state alongside the
 *  localStorage token so the rest of the app can react to login/logout
 *  without having to re-fetch /me on every render. */

import {
  createContext,
  ReactNode,
  useCallback,
  useContext,
  useEffect,
  useState,
} from "react";
import { getToken, me, setToken, User } from "./api";

interface AuthState {
  user: User | null;
  loading: boolean;
  refresh: () => Promise<void>;
  signOut: () => void;
  applyToken: (token: string, user: User) => void;
}

const AuthContext = createContext<AuthState | null>(null);

export function AuthProvider({ children }: { children: ReactNode }) {
  const [user, setUser] = useState<User | null>(null);
  const [loading, setLoading] = useState<boolean>(Boolean(getToken()));

  const refresh = useCallback(async () => {
    if (!getToken()) {
      setUser(null);
      setLoading(false);
      return;
    }
    try {
      const data = await me();
      setUser(data);
    } catch {
      setToken(null);
      setUser(null);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const signOut = useCallback(() => {
    setToken(null);
    setUser(null);
  }, []);

  const applyToken = useCallback((token: string, nextUser: User) => {
    setToken(token);
    setUser(nextUser);
    setLoading(false);
  }, []);

  return (
    <AuthContext.Provider
      value={{ user, loading, refresh, signOut, applyToken }}
    >
      {children}
    </AuthContext.Provider>
  );
}

export function useAuth(): AuthState {
  const ctx = useContext(AuthContext);
  if (!ctx) throw new Error("useAuth must be used inside <AuthProvider>");
  return ctx;
}
