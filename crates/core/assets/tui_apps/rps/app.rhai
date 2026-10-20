// The opponent cycles rock, paper, scissors so rounds can be previewed reproducibly.
fn init() {
    #{ wins: 0, losses: 0, draws: 0, rounds: 0, last: "Choose a hand", player: "?", opponent: "?" }
}

fn update(state, event) {
    if event.type == "key" {
        if event.key == "r" { return update(state, #{ type: "button", action: "rock" }); }
        if event.key == "p" { return update(state, #{ type: "button", action: "paper" }); }
        if event.key == "s" { return update(state, #{ type: "button", action: "scissors" }); }
    }
    if event.type != "button" { return state; }
    if event.action == "reset" { return init(); }
    let hands = ["rock", "paper", "scissors"];
    let player = hands.index_of(event.action);
    if player < 0 { return state; }
    let opponent = state.rounds % 3;
    state.player = ["R", "P", "S"][player];
    state.opponent = ["R", "P", "S"][opponent];
    state.rounds += 1;
    if player == opponent {
        state.draws += 1;
        state.last = "Draw";
    } else if (player + 2) % 3 == opponent {
        state.wins += 1;
        state.last = "You win";
    } else {
        state.losses += 1;
        state.last = "You lose";
    }
    state
}

fn view(state) {
    #{ title: "Rock · Paper · Scissors", view: [
        #{ type: "buttons", items: [
            #{ label: "Rock (r)", action: "rock" },
            #{ label: "Paper (p)", action: "paper" },
            #{ label: "Scissors (s)", action: "scissors" }
        ] },
        #{ type: "kv", items: [
            ["Wins", "{{wins}}"], ["Losses", "{{losses}}"],
            ["Draws", "{{draws}}"], ["Round", "{{rounds}}"] ] },
        #{ type: "text", text: state.last, tone: "accent" },
        #{ type: "text", text: "You / CPU" },
        #{ type: "canvas", rows: 2, cells: [
            [1, 0, state.player, "success"], [7, 0, state.opponent, "warning"],
            [0, 1, "[", "muted"], [2, 1, "]", "muted"],
            [6, 1, "[", "muted"], [8, 1, "]", "muted"] ] },
        #{ type: "buttons", items: [#{ label: "Reset", action: "reset" }] }
    ] }
}
