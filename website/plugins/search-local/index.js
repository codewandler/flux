const crypto = require('crypto');
const fs = require('fs');
const path = require('path');
const searchLocalModule = require('@easyops-cn/docusaurus-search-local');

const searchLocal = searchLocalModule.default || searchLocalModule;

class PathIndependentModuleIdsPlugin {
  constructor(siteDir) {
    this.siteDir = siteDir;
  }

  apply(compiler) {
    compiler.hooks.compilation.tap('PathIndependentModuleIdsPlugin', (compilation) => {
      compilation.hooks.moduleIds.tap('PathIndependentModuleIdsPlugin', (modules) => {
        const assigned = new Set();
        for (const module of modules) {
          if (!module.needId) continue;
          if (compilation.chunkGraph.getModuleId(module) !== null) continue;
          const inChunk = compilation.chunkGraph.getNumberOfModuleChunks(module) !== 0;
          const buildMeta = module.buildMeta || {};
          if (!inChunk && !buildMeta.isCssModule && !buildMeta.needIdInConcatenation) {
            continue;
          }
          const identifier = module.identifier().split(this.siteDir).join('<site>');
          const id = crypto.createHash('sha256').update(identifier).digest('hex').slice(0, 16);
          if (assigned.has(id)) throw new Error(`portable module id collision: ${id}`);
          assigned.add(id);
          compilation.chunkGraph.setModuleId(module, id);
        }
      });
    });
  }
}

/**
 * The upstream theme writes `require.resolve` results into generated client modules. Webpack then
 * hashes those absolute requests, while Docusaurus code generation and Babel do the same for client
 * modules and helper imports. Bare requests and root-normalized IDs resolve the same files without
 * encoding where npm installed them.
 */
module.exports = function pathIndependentSearchLocal(context, options) {
  const plugin = searchLocal(context, options);
  const generatedDir = path.join(
    context.generatedFilesDir,
    '@easyops-cn/docusaurus-search-local',
    'default',
  );
  const nodeModulesPrefix = `${context.siteDir}${path.sep}node_modules${path.sep}`;

  for (const name of ['generated.js', 'generated-constants.js']) {
    const file = path.join(generatedDir, name);
    const source = fs.readFileSync(file, 'utf8');
    fs.writeFileSync(file, source.split(nodeModulesPrefix).join(''));
  }

  const configureWebpack = plugin.configureWebpack;
  plugin.configureWebpack = function configurePathIndependentWebpack(...args) {
    const clientModules = path.join(context.generatedFilesDir, 'client-modules.js');
    const clientSource = fs
      .readFileSync(clientModules, 'utf8')
      .split(nodeModulesPrefix)
      .join('')
      .split(`${context.siteDir}${path.sep}`)
      .join('../');
    fs.writeFileSync(clientModules, clientSource);

    const docsDependency = path.join(
      context.generatedFilesDir,
      'docusaurus-plugin-content-docs',
      'default',
      '__mdx-loader-dependency.json',
    );
    const dependencySource = fs
      .readFileSync(docsDependency, 'utf8')
      .split(context.siteDir)
      .join('<site>');
    fs.writeFileSync(docsDependency, dependencySource);

    const upstream = configureWebpack ? configureWebpack.apply(plugin, args) || {} : {};
    return {
      ...upstream,
      optimization: {...upstream.optimization, moduleIds: false},
      plugins: [...(upstream.plugins || []), new PathIndependentModuleIdsPlugin(context.siteDir)],
    };
  };

  return plugin;
};

module.exports.validateOptions = searchLocalModule.validateOptions;
