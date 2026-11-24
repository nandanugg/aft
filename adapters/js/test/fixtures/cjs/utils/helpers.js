const findItems = async (id) => id;

function waitFor(id) {
  return findItems(id);
}

module.exports = { findItems, waitFor, inline: () => 1 };
