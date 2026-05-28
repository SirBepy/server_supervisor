import "@phosphor-icons/web";
import "./styles/base.css";
import { mountDashboard } from "./views/dashboard/dashboard";

const app = document.getElementById("app")!;
mountDashboard(app);
