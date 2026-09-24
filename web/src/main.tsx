import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createBrowserRouter, RouterProvider } from "react-router";
import { ApiError } from "@/api";
import { Layout } from "@/components/Layout";
import { Login } from "@/pages/Login";
import { Enrol } from "@/pages/Enrol";
import { Overview } from "@/pages/Overview";
import { Account } from "@/pages/Account";
import { Instrument } from "@/pages/Instrument";
import { Alerts } from "@/pages/Alerts";
import { Settings } from "@/pages/Settings";
import "./index.css";

// shadcn's dark variant keys off a `.dark` class; follow the system setting.
const darkQuery = window.matchMedia("(prefers-color-scheme: dark)");
const applyDark = () => document.documentElement.classList.toggle("dark", darkQuery.matches);
applyDark();
darkQuery.addEventListener("change", applyDark);

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      staleTime: 5_000,
      retry: (n, e) => !(e instanceof ApiError && e.status < 500) && n < 2,
    },
  },
});

const router = createBrowserRouter([
  { path: "/login", element: <Login /> },
  { path: "/enrol", element: <Enrol /> },
  {
    element: <Layout />,
    children: [
      { path: "/", element: <Overview /> },
      { path: "/accounts/:id", element: <Account /> },
      { path: "/instruments/:id", element: <Instrument /> },
      { path: "/alerts", element: <Alerts /> },
      { path: "/settings", element: <Settings /> },
    ],
  },
]);

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>
  </StrictMode>,
);
