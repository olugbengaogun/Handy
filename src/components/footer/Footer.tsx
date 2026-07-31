import React, { useState, useEffect } from "react";
import { getVersion } from "@tauri-apps/api/app";
import { openUrl } from "@tauri-apps/plugin-opener";

import ModelSelector from "../model-selector";
import UpdateChecker from "../update-checker";

const Footer: React.FC = () => {
  const [version, setVersion] = useState("");

  useEffect(() => {
    const fetchVersion = async () => {
      try {
        const appVersion = await getVersion();
        setVersion(appVersion);
      } catch (error) {
        console.error("Failed to get app version:", error);
        setVersion("0.1.2");
      }
    };

    fetchVersion();
  }, []);

  const handleAuthorClick = () => {
    openUrl("https://www.gbengaogun.com/handy-plus").catch((error) => {
      console.error("Failed to open author website:", error);
    });
  };

  return (
    <div className="w-full border-t border-mid-gray/20 pt-3">
      <div className="flex justify-between items-center text-xs px-4 pb-3 text-text/60">
        <div className="flex items-center gap-4">
          <ModelSelector />
        </div>

        {/* Credit + Update Status */}
        {/* eslint-disable i18next/no-literal-string -- fixed attribution, not user-facing copy to localize */}
        <div className="flex items-center gap-1">
          <span>
            Built by{" "}
            <button
              onClick={handleAuthorClick}
              className="underline hover:text-text/90 cursor-pointer"
            >
              Olugbenga Ogunbowale
            </button>{" "}
            with support from CJ Pais
          </span>
          {/* eslint-enable i18next/no-literal-string */}
          <span>•</span>
          <UpdateChecker />
          <span>•</span>
          {/* eslint-disable-next-line i18next/no-literal-string */}
          <span>v{version}</span>
        </div>
      </div>
    </div>
  );
};

export default Footer;
