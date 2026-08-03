const docusaurusPresetModule = require('@docusaurus/babel/preset');

const docusaurusPreset = docusaurusPresetModule.default || docusaurusPresetModule;

module.exports = function pathIndependentBabel(api, options) {
  const config = docusaurusPreset(api, options);
  for (const plugin of config.plugins) {
    if (Array.isArray(plugin) && plugin[0].includes('@babel/plugin-transform-runtime')) {
      plugin[1].absoluteRuntime = false;
    }
  }
  return config;
};
