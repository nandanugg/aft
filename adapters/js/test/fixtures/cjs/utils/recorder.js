let last = null;

module.exports = {
  record: (entry) => {
    last = entry;
  },
  reset: () => {
    last = null;
  },
};
